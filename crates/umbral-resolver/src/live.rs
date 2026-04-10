//! Live PyPI package source — bridges the async `SimpleApiClient` into the
//! synchronous `PackageSource` trait expected by the PubGrub provider.
//!
//! Version lists and metadata are fetched on demand from the PyPI Simple API
//! and cached in a `DashMap` so repeated queries for the same package hit
//! memory instead of the network.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use dashmap::DashMap;
use tokio::runtime::Handle;
use tracing::{debug, warn};

use umbral_pep440::{PackageName, Version, VersionSpecifiers};
use umbral_pep508::Requirement;
use umbral_pypi_client::{DistributionFile, Metadata, SimpleApiClient};

use crate::provider::{DistributionFileInfo, PackageMetadata, PackageSource};

/// Cached data for a single package: its available versions plus per-version
/// metadata (lazily populated).
#[derive(Debug)]
struct CachedProject {
    /// (Version, DistributionFile) pairs, sorted newest-first.
    versions: Vec<(Version, DistributionFile)>,
    /// Metadata cache keyed by version string.
    metadata: HashMap<String, Option<PackageMetadata>>,
}

/// A [`PackageSource`] backed by a live PyPI index.
///
/// Wraps a [`SimpleApiClient`] and uses a tokio `Handle` to drive async
/// fetches from inside the synchronous PubGrub resolution loop. Results
/// are cached in memory so each package/version pair is fetched at most once.
///
/// The `cache` and `sdist_only_packages` fields are wrapped in `Arc` so that
/// clones (e.g. the per-target clones in `resolve_universal`) share the same
/// in-memory state and avoid redundant network fetches.
#[derive(Clone)]
pub struct LivePypiSource {
    client: Arc<SimpleApiClient>,
    handle: Handle,
    cache: Arc<DashMap<String, CachedProject>>,
    /// Packages that had files on PyPI but zero wheels (sdist-only).
    /// Used to produce better error messages when resolution fails.
    sdist_only_packages: Arc<Mutex<HashSet<String>>>,
}

impl LivePypiSource {
    /// Create a new live source.
    ///
    /// `handle` must be a handle to a running tokio runtime (typically
    /// obtained via `Handle::current()` inside an async context).
    pub fn new(client: Arc<SimpleApiClient>, handle: Handle) -> Self {
        Self {
            client,
            handle,
            cache: Arc::new(DashMap::new()),
            sdist_only_packages: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Ensure the project page for `name` is cached. Returns nothing;
    /// callers read from the cache afterwards.
    ///
    /// # Concurrency
    ///
    /// Uses an early `contains_key` check to avoid fetching when the cache is
    /// already populated, then uses `entry().or_insert()` when writing. Two
    /// threads may both pass the initial check and double-fetch, but the
    /// `or_insert` ensures only the first result is stored, preventing data
    /// loss. A true single-flight mechanism (e.g., `tokio::sync::OnceCell`
    /// per project) would eliminate the redundant fetch but adds complexity;
    /// this is an acceptable trade-off for now.
    fn ensure_project(&self, name: &str) {
        // Fast path: already cached.
        if self.cache.contains_key(name) {
            return;
        }

        debug!(package = name, "fetching project page from index");

        let client = Arc::clone(&self.client);
        let name_owned = name.to_string();

        let result = tokio::task::block_in_place(|| {
            self.handle
                .block_on(async { client.fetch_project_page(&name_owned).await })
        });

        let project_data = match result {
            Ok(page) => {
                // Filter to wheels only (sdists don't expose PEP 658 metadata)
                // and deduplicate by version, keeping the first (preferred) file
                // for each version.
                let project_files_count = page.files.len();
                let mut seen: HashMap<String, (Version, DistributionFile)> = HashMap::new();

                for file in page.files {
                    // Skip yanked distributions
                    if file.yanked.is_some() {
                        continue;
                    }

                    // Only consider wheels — we need PEP 658 metadata
                    if !file.filename.ends_with(".whl") {
                        continue;
                    }

                    let whl = match umbral_pypi_client::WheelFilename::parse(&file.filename) {
                        Ok(w) => w,
                        Err(e) => {
                            warn!(filename = %file.filename, error = %e, "skipping unparseable wheel");
                            continue;
                        }
                    };

                    let version: Version = match whl.version.parse() {
                        Ok(v) => v,
                        Err(e) => {
                            warn!(version = %whl.version, error = %e, "skipping unparseable version");
                            continue;
                        }
                    };

                    // Keep the best file per version for metadata fetching.
                    // Prefer pure-Python wheels (py3-none-any) whose metadata is
                    // platform-independent, and files with PEP 658 dist-info-metadata.
                    // All wheels are kept in the version list regardless of platform;
                    // the download-time wheel selector handles platform compatibility.
                    seen.entry(version.to_string())
                        .and_modify(|(_, existing)| {
                            if wheel_metadata_priority(&file) > wheel_metadata_priority(existing) {
                                *existing = file.clone();
                            }
                        })
                        .or_insert((version, file));
                }

                let mut versions: Vec<(Version, DistributionFile)> = seen.into_values().collect();
                // Sort newest first
                versions.sort_by(|a, b| b.0.cmp(&a.0));

                // Warn when a package has files on PyPI but zero wheels
                // (sdist-only packages). Store this for better error reporting.
                if versions.is_empty() && project_files_count > 0 {
                    tracing::warn!(
                        "Package '{}' has no wheel distributions available. \
                         Only source distributions (sdists) were found, which are not yet supported. \
                         Consider installing a version that provides wheels.",
                        name
                    );
                    if let Ok(mut set) = self.sdist_only_packages.lock() {
                        set.insert(name.to_string());
                    }
                }

                CachedProject {
                    versions,
                    metadata: HashMap::new(),
                }
            }
            Err(e) => {
                warn!(package = name, error = %e, "failed to fetch project page");
                // Insert empty entry so we don't retry
                CachedProject {
                    versions: vec![],
                    metadata: HashMap::new(),
                }
            }
        };

        // Use entry API to avoid overwriting if another thread inserted first.
        // This prevents data loss from the TOCTOU race between the initial
        // contains_key check and this insert.
        self.cache.entry(name.to_string()).or_insert(project_data);
    }

    /// Check if a package was detected as sdist-only (has files on PyPI
    /// but zero wheels). Used for producing better error messages.
    pub fn is_sdist_only(&self, name: &str) -> bool {
        self.sdist_only_packages
            .lock()
            .map(|set| set.contains(name))
            .unwrap_or(false)
    }

    /// Return all packages detected as sdist-only.
    pub fn sdist_only_packages(&self) -> HashSet<String> {
        self.sdist_only_packages
            .lock()
            .map(|set| set.clone())
            .unwrap_or_default()
    }

    /// Fetch and parse metadata for a specific distribution file, returning
    /// a `PackageMetadata` ready for the resolver.
    fn fetch_metadata_for(&self, file: &DistributionFile) -> Option<PackageMetadata> {
        let client = Arc::clone(&self.client);
        let file = file.clone();

        let result = tokio::task::block_in_place(|| {
            self.handle
                .block_on(async { client.fetch_metadata(&file).await })
        });

        match result {
            Ok(meta) => Some(convert_metadata(&meta)),
            Err(e) => {
                warn!(filename = %file.filename, error = %e, "failed to fetch metadata");
                None
            }
        }
    }
}

impl PackageSource for LivePypiSource {
    fn available_versions(&self, package: &PackageName) -> Vec<Version> {
        let name = package.as_str();
        self.ensure_project(name);

        let entry = match self.cache.get(name) {
            Some(e) => e,
            None => return vec![],
        };

        entry.versions.iter().map(|(v, _)| v.clone()).collect()
    }

    fn get_metadata(&self, package: &PackageName, version: &Version) -> Option<PackageMetadata> {
        let name = package.as_str();
        self.ensure_project(name);

        let version_str = version.to_string();

        // Check the metadata cache first.
        {
            let entry = self.cache.get(name)?;
            if let Some(cached) = entry.metadata.get(&version_str) {
                return cached.clone();
            }
        }

        // Find the distribution file for this version.
        let file = {
            let entry = self.cache.get(name)?;
            entry
                .versions
                .iter()
                .find(|(v, _)| v == version)
                .map(|(_, f)| f.clone())
        };

        let file = file?;
        let meta = self.fetch_metadata_for(&file);

        // Cache the result (even if None, to avoid retries).
        if let Some(mut entry) = self.cache.get_mut(name) {
            entry.metadata.insert(version_str, meta.clone());
        }

        meta
    }

    fn sdist_only_packages(&self) -> HashSet<String> {
        self.sdist_only_packages
            .lock()
            .map(|set| set.clone())
            .unwrap_or_default()
    }

    fn distribution_files(
        &self,
        package: &PackageName,
        version: &Version,
    ) -> Vec<DistributionFileInfo> {
        let name = package.as_str();
        let entry = match self.cache.get(name) {
            Some(e) => e,
            None => return vec![],
        };

        entry
            .versions
            .iter()
            .filter(|(v, _)| v == version)
            .map(|(_, f)| DistributionFileInfo {
                filename: f.filename.clone(),
                url: f.url.clone(),
                hash: f.hashes.get("sha256").cloned(),
                size: None,
            })
            .collect()
    }
}

/// Compute a priority score for a wheel file for metadata fetching purposes.
///
/// Higher scores are preferred. Pure-Python wheels are preferred because their
/// metadata is platform-independent (always valid regardless of the host system).
/// Files with PEP 658 `dist-info-metadata` are also preferred since they allow
/// direct metadata access without downloading the whole wheel.
fn wheel_metadata_priority(file: &DistributionFile) -> u8 {
    let mut score = 0u8;

    // Prefer files with PEP 658 metadata available (avoids full wheel download)
    if file.dist_info_metadata.is_some() {
        score += 2;
    }

    // Prefer pure-Python wheels — their metadata is platform-independent
    if let Ok(wf) = umbral_pypi_client::WheelFilename::parse(&file.filename) {
        if wf.abi_tag == "none" && wf.platform_tag == "any" {
            score += 4;
        }
    }

    score
}

/// Convert a raw `Metadata` (from the PyPI client) into the resolver's
/// `PackageMetadata` by parsing PEP 508 requirement strings.
///
/// `Requires-Dist` lines may contain `; extra == "name"` markers.  Because
/// the pep508 marker parser does not recognise `extra` as a variable, we
/// pre-process each line: if a bare `extra == "name"` condition is present
/// we strip it, parse the remainder normally, and file the requirement
/// under the extras map.
fn convert_metadata(meta: &Metadata) -> PackageMetadata {
    let mut dependencies = Vec::new();
    let mut extras: HashMap<String, Vec<Requirement>> = HashMap::new();

    for req_str in &meta.requires_dist {
        // Try to detect and strip `; extra == "name"` (possibly combined
        // with other markers via `and` or `or`).
        let stripped = strip_extra_marker_full(req_str);

        match Requirement::parse(&stripped.cleaned) {
            Ok(req) => {
                if let Some(name) = stripped.extra_name {
                    extras.entry(name).or_default().push(req.clone());

                    // When the original marker contained `or` with non-extra
                    // clauses, the requirement should ALSO be a regular
                    // dependency with those residual clauses as its marker.
                    // e.g., `extra == "security" or python_version >= "3.8"`
                    // means: install if security extra is active OR python >= 3.8.
                    if let Some(ref residual_marker) = stripped.residual_or_marker {
                        let base = req_str.split(';').next().unwrap_or(req_str).trim();
                        let residual_req_str = format!("{} ; {}", base, residual_marker);
                        match Requirement::parse(&residual_req_str) {
                            Ok(residual_req) => {
                                dependencies.push(residual_req);
                            }
                            Err(e) => {
                                warn!(
                                    requirement = %residual_req_str,
                                    error = %e,
                                    "skipping unparseable residual or-marker requirement"
                                );
                            }
                        }
                    }
                } else {
                    dependencies.push(req);
                }
            }
            Err(e) => {
                warn!(requirement = %req_str, error = %e, "skipping unparseable requirement");
            }
        }
    }

    let requires_python =
        meta.requires_python
            .as_ref()
            .and_then(|s| match s.parse::<VersionSpecifiers>() {
                Ok(specs) => Some(specs),
                Err(e) => {
                    warn!(requires_python = %s, error = %e, "skipping unparseable requires-python");
                    None
                }
            });

    PackageMetadata {
        dependencies,
        requires_python,
        yanked: false,
        extras,
    }
}

/// Result of stripping extra markers from a requirement string.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StrippedExtra {
    /// The requirement string with the `extra == "name"` clause removed.
    pub cleaned: String,
    /// The name of the extra, if found.
    pub extra_name: Option<String>,
    /// When the marker contained `or`, the non-extra clauses that should
    /// also cause this requirement to be installed as a regular dependency.
    /// For example, `extra == "security" or python_version >= "3.8"` yields
    /// `residual_or_marker = Some("python_version >= \"3.8\"")`.
    pub residual_or_marker: Option<String>,
}

/// Try to extract an `extra == "name"` condition from a requirement string.
///
/// Returns `(cleaned_requirement, Some(extra_name))` if found, or
/// `(original, None)` otherwise.  The cleaned string has the `extra`
/// clause removed so that the remaining text can be parsed by the
/// pep508 parser (which does not understand the `extra` variable).
#[allow(dead_code)] // Used by tests in lib.rs
pub(crate) fn strip_extra_marker(req: &str) -> (String, Option<String>) {
    let result = strip_extra_marker_full(req);
    (result.cleaned, result.extra_name)
}

/// Full version of `strip_extra_marker` that also returns residual `or` markers.
pub(crate) fn strip_extra_marker_full(req: &str) -> StrippedExtra {
    // Quick check — avoid the regex-like work for the common case.
    if !req.contains("extra") {
        return StrippedExtra {
            cleaned: req.to_string(),
            extra_name: None,
            residual_or_marker: None,
        };
    }

    // Split at the first semicolon to separate the requirement from the marker.
    let Some(semi_pos) = req.find(';') else {
        return StrippedExtra {
            cleaned: req.to_string(),
            extra_name: None,
            residual_or_marker: None,
        };
    };

    let base = &req[..semi_pos];
    let marker_str = req[semi_pos + 1..].trim();

    // Try to find `extra == "name"` or `extra == 'name'` in the marker.
    let extra_name = extract_extra_value(marker_str);
    let Some(ref extra_name) = extra_name else {
        // The marker mentions "extra" in some way we don't handle.
        // Return the original string so the caller can try parsing it
        // (it will likely fail and be warned about).
        return StrippedExtra {
            cleaned: req.to_string(),
            extra_name: None,
            residual_or_marker: None,
        };
    };

    // Remove the `extra == "name"` clause from the marker.  If there
    // are remaining clauses (joined by `and`), re-attach them.
    let (remaining_marker, residual_or_marker) = remove_extra_clause_with_or(marker_str);

    let cleaned = if remaining_marker.is_empty() {
        base.trim_end().to_string()
    } else {
        format!("{} ; {}", base.trim_end(), remaining_marker)
    };

    StrippedExtra {
        cleaned,
        extra_name: Some(extra_name.clone()),
        residual_or_marker,
    }
}

/// Extract the quoted extra name from a marker string that contains
/// `extra == "name"` (or single-quoted), or the reversed form `"name" == extra`.
fn extract_extra_value(marker: &str) -> Option<String> {
    // Try the standard form: `extra == "name"`
    if let Some(val) = extract_extra_value_standard(marker) {
        return Some(val);
    }

    // Try the reversed form: `"name" == extra`
    extract_extra_value_reversed(marker)
}

/// Extract extra value from standard form: `extra == "name"`.
fn extract_extra_value_standard(marker: &str) -> Option<String> {
    let extra_pos = marker.find("extra")?;
    let after_extra = marker[extra_pos + 5..].trim_start();
    let after_eq = after_extra.strip_prefix("==")?.trim_start();

    let (quote, rest) = if let Some(rest) = after_eq.strip_prefix('"') {
        ('"', rest)
    } else if let Some(rest) = after_eq.strip_prefix('\'') {
        ('\'', rest)
    } else {
        return None;
    };

    let end = rest.find(quote)?;
    Some(rest[..end].to_string())
}

/// Extract extra value from reversed form: `"name" == extra`.
fn extract_extra_value_reversed(marker: &str) -> Option<String> {
    // Look for a pattern like `"name" == extra` or `'name' == extra`
    let eq_pos = marker.find("==")?;
    let before_eq = marker[..eq_pos].trim();
    let after_eq = marker[eq_pos + 2..].trim();

    // The after side must be exactly "extra" (not "extra_field" etc.)
    if after_eq != "extra" {
        return None;
    }

    // The before side must be a quoted string
    let (quote, inner) = if before_eq.starts_with('"') && before_eq.ends_with('"') {
        ('"', &before_eq[1..before_eq.len() - 1])
    } else if before_eq.starts_with('\'') && before_eq.ends_with('\'') {
        ('\'', &before_eq[1..before_eq.len() - 1])
    } else {
        return None;
    };
    let _ = quote;

    Some(inner.to_string())
}

/// Remove the `extra == "..."` clause from a marker string.
///
/// Handles the common patterns:
/// - `extra == "name"` (only clause)
/// - `extra == "name" and <rest>`
/// - `<rest> and extra == "name"`
/// - `"name" == extra` (reversed operand)
/// - Clauses joined by `or` (the full marker is kept as-is when `or`
///   is involved, since removing just the extra clause would change
///   semantics)
/// - Outer parentheses are stripped before splitting
///
/// **Limitation**: Fully nested parenthesized expressions like
///   `(extra == "a" and python_version >= "3.8") or (extra == "b")`
///   are not handled — these would require a proper marker tree walker.
///   When such an expression is encountered, the clause splitting operates
///   on the raw string which may produce incorrect results (e.g. losing
///   inner parenthesized groups). A `tracing::warn!` is emitted so this
///   is visible in verbose output.
#[allow(dead_code)]
fn remove_extra_clause(marker: &str) -> String {
    remove_extra_clause_with_or(marker).0
}

/// Like `remove_extra_clause`, but also returns the non-extra `or` clauses
/// when the marker contains `or`.
///
/// Returns `(remaining_marker_for_extra, residual_or_clauses)`:
/// - `remaining_marker_for_extra`: used as the marker for the extra-filed req
///   (empty string when `or` is present, since the extra req gets no marker).
/// - `residual_or_clauses`: `Some(...)` when there are non-extra `or` clauses
///   that should also cause the requirement to be installed as a regular dep.
fn remove_extra_clause_with_or(marker: &str) -> (String, Option<String>) {
    let marker = marker.trim();

    // Strip balanced outer parentheses (e.g., `(extra == "x" and ...)`)
    let marker = strip_outer_parens(marker);

    // Detect nested parenthesized sub-expressions that we cannot split
    // correctly with simple string splitting. Warn so the limitation is
    // visible in verbose output rather than silently producing wrong results.
    if marker.contains('(') {
        warn!(
            marker,
            "marker contains nested parenthesized expressions; \
             extra-clause removal may produce incorrect results — \
             a proper marker tree walker is needed to handle this"
        );
    }

    if marker.contains(" or ") {
        // Split on ` or ` and collect non-extra clauses.
        let or_parts: Vec<&str> = marker.split(" or ").collect();
        let non_extra_parts: Vec<&str> = or_parts
            .iter()
            .filter(|p| !is_extra_clause(strip_outer_parens(p.trim())))
            .copied()
            .collect();

        let residual = if non_extra_parts.is_empty() {
            None
        } else {
            Some(non_extra_parts.join(" or "))
        };

        // The extra-filed requirement gets no additional marker (empty string).
        // The residual or-clauses are returned separately for dual-filing.
        return (String::new(), residual);
    }

    // Split on ` and ` and remove any clause that mentions `extra`.
    let parts: Vec<&str> = marker.split(" and ").collect();
    let filtered: Vec<&str> = parts
        .into_iter()
        .filter(|p| !is_extra_clause(p.trim()))
        .collect();
    (filtered.join(" and "), None)
}

/// Check whether a single clause is an `extra` comparison.
///
/// Matches both `extra == "..."` and `"..." == extra` forms,
/// including with surrounding parentheses.
///
/// The check requires "extra" to be followed by whitespace or an operator
/// character (not more word characters), preventing false positives on
/// names like `extra_field`.
fn is_extra_clause(clause: &str) -> bool {
    let clause = strip_outer_parens(clause.trim());
    // Standard form: starts with `extra` followed by non-word char or end
    if let Some(rest) = clause.strip_prefix("extra") {
        if rest.is_empty()
            || rest.starts_with(' ')
            || rest.starts_with('=')
            || rest.starts_with('!')
            || rest.starts_with('<')
            || rest.starts_with('>')
        {
            return true;
        }
    }
    // Reversed form: ends with `extra` after `==`
    if let Some(eq_pos) = clause.find("==") {
        let after_eq = clause[eq_pos + 2..].trim();
        if after_eq == "extra" {
            return true;
        }
    }
    false
}

/// Strip balanced outer parentheses from a string.
/// `"(foo and bar)"` -> `"foo and bar"`, `"(foo) and (bar)"` -> unchanged.
fn strip_outer_parens(s: &str) -> &str {
    let s = s.trim();
    if !s.starts_with('(') || !s.ends_with(')') {
        return s;
    }

    // Verify the parens are actually balanced at the outer level.
    // We need to check that the opening '(' matches the closing ')'.
    let inner = &s[1..s.len() - 1];
    let mut depth = 0i32;
    for ch in inner.chars() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    // The closing ')' at the end is not the match for our opening '('.
                    return s;
                }
            }
            _ => {}
        }
    }

    if depth == 0 {
        inner
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clone_shares_cache() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = Arc::new(
            SimpleApiClient::new(
                url::Url::parse("https://pypi.org/simple/").unwrap(),
                std::env::temp_dir().join("umbral-test-cache-share"),
            )
            .unwrap(),
        );
        let source = LivePypiSource::new(client, rt.handle().clone());

        // Insert a dummy entry into the cache.
        source.cache.insert(
            "test-pkg".to_string(),
            CachedProject {
                versions: vec![],
                metadata: HashMap::new(),
            },
        );

        // Clone the source and verify it sees the same cache entry.
        let cloned = source.clone();
        assert!(
            cloned.cache.contains_key("test-pkg"),
            "cloned source must share the same cache"
        );

        // Insert via the clone and verify the original sees it.
        cloned.cache.insert(
            "other-pkg".to_string(),
            CachedProject {
                versions: vec![],
                metadata: HashMap::new(),
            },
        );
        assert!(
            source.cache.contains_key("other-pkg"),
            "original source must see entries inserted by clone"
        );

        // Verify sdist_only_packages is also shared.
        source
            .sdist_only_packages
            .lock()
            .unwrap()
            .insert("sdist-pkg".to_string());
        assert!(
            cloned
                .sdist_only_packages
                .lock()
                .unwrap()
                .contains("sdist-pkg"),
            "cloned source must share sdist_only_packages"
        );
    }
}
