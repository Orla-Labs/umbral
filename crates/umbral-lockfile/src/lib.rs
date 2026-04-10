//! `uv.lock` — read and write uv-compatible lockfiles.
//!
//! This crate provides a data model for the `uv.lock` format, with support for:
//! - Parsing existing `uv.lock` files (so users can switch from uv to Umbral)
//! - Writing `uv.lock` files (so Umbral's output works with uv)
//! - Staleness detection via input hashing
//! - PEP 503 normalized package lookups
//!
//! The `uv.lock` TOML uses dotted subtables inside `[[package]]` arrays
//! (e.g. `[package.optional-dependencies]`) followed by sibling keys like
//! `sdist` and `wheels`. The standard `toml` serde crate cannot handle
//! keys appearing after dotted subtables, so we use `toml_edit` for
//! parsing and manual formatting for writing.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

use sha2::{Digest, Sha256};
use thiserror::Error;
use toml_edit::{DocumentMut, Item, Table, Value};

// ── Errors ──────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum LockError {
    #[error("failed to read {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },

    #[error("failed to parse lockfile: {0}")]
    Parse(String),

    #[error("failed to serialize lockfile: {0}")]
    Serialize(String),

    #[error("invalid lockfile: {0}")]
    Invalid(String),
}

// Keep the old name as an alias for downstream compatibility during migration.
pub type LockfileError = LockError;

// ── PackageSource ───────────────────────────────────────────────────

/// Source of a locked package.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum PackageSource {
    Registry { url: String },
    Git { url: String },
    Path { path: String },
    Directory { path: String },
    Editable { path: String },
    Virtual { path: String },
}

impl Default for PackageSource {
    fn default() -> Self {
        PackageSource::Registry {
            url: "https://pypi.org/simple".to_string(),
        }
    }
}

// ── Dependency ──────────────────────────────────────────────────────

/// A dependency reference within a locked package.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Dependency {
    pub name: String,
    pub version: Option<String>,
    pub source: Option<PackageSource>,
    pub marker: Option<String>,
    pub extra: Option<Vec<String>>,
}

// ── Artifact ────────────────────────────────────────────────────────

/// A downloadable artifact (sdist or wheel).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Artifact {
    pub url: Option<String>,
    pub path: Option<String>,
    pub filename: Option<String>,
    pub hash: String,
    pub size: Option<u64>,
}

// ── LockOptions ─────────────────────────────────────────────────────

/// Options section of the lockfile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LockOptions {
    pub resolution_mode: Option<String>,
    pub prerelease_mode: Option<String>,
    /// Marker expressions defining the target environments for universal resolution.
    /// When present, the lockfile is a universal (cross-platform) lockfile.
    pub resolution_markers: Option<Vec<String>>,
}

// ── LockedPackage ───────────────────────────────────────────────────

/// A single locked package in the uv.lock format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedPackage {
    pub name: String,
    pub version: Option<String>,
    pub source: PackageSource,
    pub dependencies: Vec<Dependency>,
    pub optional_dependencies: BTreeMap<String, Vec<Dependency>>,
    pub dev_dependencies: BTreeMap<String, Vec<Dependency>>,
    pub sdist: Option<Artifact>,
    pub wheels: Vec<Artifact>,
}

impl PartialOrd for LockedPackage {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for LockedPackage {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.name.cmp(&other.name)
    }
}

// ── UvLock ──────────────────────────────────────────────────────────

/// Top-level representation of a `uv.lock` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UvLock {
    pub version: u32,
    pub revision: u32,
    pub requires_python: Option<String>,
    pub options: LockOptions,
    pub packages: Vec<LockedPackage>,
}

impl FromStr for UvLock {
    type Err = LockError;

    fn from_str(content: &str) -> Result<Self, Self::Err> {
        let doc: DocumentMut = content
            .parse()
            .map_err(|e| LockError::Parse(format!("{}", e)))?;

        let version = doc.get("version").and_then(|v| v.as_integer()).unwrap_or(1) as u32;

        let revision = doc
            .get("revision")
            .and_then(|v| v.as_integer())
            .unwrap_or(0) as u32;

        let requires_python = doc
            .get("requires-python")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let options = parse_options(doc.get("options"));

        // Accept either `package` or `distribution` as the table array name.
        let pkg_array = doc.get("package").or_else(|| doc.get("distribution"));

        let packages = match pkg_array {
            Some(Item::ArrayOfTables(arr)) => arr
                .iter()
                .map(parse_package)
                .collect::<Result<Vec<_>, _>>()?,
            _ => vec![],
        };

        Ok(UvLock {
            version,
            revision,
            requires_python,
            options,
            packages,
        })
    }
}

impl UvLock {
    // ── Parsing ─────────────────────────────────────────────────

    /// Parse a `uv.lock` TOML string into the data model.
    ///
    /// Uses `toml_edit` to handle the uv.lock format which places keys
    /// after dotted subtables (e.g. `sdist` after `[package.optional-dependencies]`).
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(content: &str) -> Result<Self, LockError> {
        <Self as FromStr>::from_str(content)
    }

    /// Read and parse a `uv.lock` file from disk.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, LockError> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path).map_err(|e| LockError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        Self::from_str(&content)
    }

    // ── Writing ─────────────────────────────────────────────────

    /// Serialize the lockfile to a deterministic TOML string matching
    /// the uv.lock format.
    pub fn to_toml(&self) -> Result<String, LockError> {
        let mut out = String::with_capacity(4096);

        // Header
        out.push_str("# This file was @generated by umbral. Do not edit manually.\n");

        // Top-level keys
        out.push_str(&format!("version = {}\n", self.version));
        out.push_str(&format!("revision = {}\n", self.revision));
        if let Some(ref rp) = self.requires_python {
            out.push_str(&format!(
                "requires-python = \"{}\"\n",
                escape_toml_string(rp)
            ));
        }

        // Options section (only if non-empty)
        let has_options = self.options.resolution_mode.is_some()
            || self.options.prerelease_mode.is_some()
            || self.options.resolution_markers.is_some();
        if has_options {
            out.push_str("\n[options]\n");
            if let Some(ref mode) = self.options.resolution_mode {
                out.push_str(&format!(
                    "resolution-mode = \"{}\"\n",
                    escape_toml_string(mode)
                ));
            }
            if let Some(ref mode) = self.options.prerelease_mode {
                out.push_str(&format!(
                    "prerelease-mode = \"{}\"\n",
                    escape_toml_string(mode)
                ));
            }
            if let Some(ref markers) = self.options.resolution_markers {
                out.push_str("resolution-markers = [\n");
                for m in markers {
                    out.push_str(&format!("    \"{}\",\n", escape_toml_string(m)));
                }
                out.push_str("]\n");
            }
        }

        // Packages — sorted by name for determinism
        let mut sorted_packages = self.packages.clone();
        sorted_packages.sort();

        for pkg in &sorted_packages {
            out.push('\n');
            out.push_str("[[package]]\n");
            out.push_str(&format!("name = \"{}\"\n", escape_toml_string(&pkg.name)));
            if let Some(ref v) = pkg.version {
                out.push_str(&format!("version = \"{}\"\n", escape_toml_string(v)));
            }
            out.push_str(&format!("source = {}\n", format_source(&pkg.source)));

            // Dependencies
            if !pkg.dependencies.is_empty() {
                let mut sorted_deps = pkg.dependencies.clone();
                sorted_deps.sort();
                out.push_str("dependencies = [\n");
                for dep in &sorted_deps {
                    out.push_str(&format!("    {},\n", format_dependency(dep)));
                }
                out.push_str("]\n");
            }

            // Optional dependencies
            if !pkg.optional_dependencies.is_empty() {
                out.push('\n');
                out.push_str("[package.optional-dependencies]\n");
                for (group, deps) in &pkg.optional_dependencies {
                    let mut sorted_deps = deps.clone();
                    sorted_deps.sort();
                    out.push_str(&format!("{} = [\n", group));
                    for dep in &sorted_deps {
                        out.push_str(&format!("    {},\n", format_dependency(dep)));
                    }
                    out.push_str("]\n");
                }
            }

            // Dev dependencies
            if !pkg.dev_dependencies.is_empty() {
                out.push('\n');
                out.push_str("[package.dev-dependencies]\n");
                for (group, deps) in &pkg.dev_dependencies {
                    let mut sorted_deps = deps.clone();
                    sorted_deps.sort();
                    out.push_str(&format!("{} = [\n", group));
                    for dep in &sorted_deps {
                        out.push_str(&format!("    {},\n", format_dependency(dep)));
                    }
                    out.push_str("]\n");
                }
            }

            // Sdist
            if let Some(ref sdist) = pkg.sdist {
                out.push('\n');
                out.push_str(&format!("sdist = {}\n", format_artifact(sdist)));
            }

            // Wheels
            if !pkg.wheels.is_empty() {
                out.push('\n');
                out.push_str("wheels = [\n");
                let mut sorted_wheels = pkg.wheels.clone();
                sorted_wheels.sort();
                for wheel in &sorted_wheels {
                    out.push_str(&format!("    {},\n", format_artifact(wheel)));
                }
                out.push_str("]\n");
            }
        }

        Ok(out)
    }

    /// Write the lockfile to disk atomically (write to temp, then rename).
    pub fn write_to(&self, path: impl AsRef<Path>) -> Result<(), LockError> {
        let content = self.to_toml()?;
        let path = path.as_ref();
        let temp = path.with_extension("lock.tmp");
        std::fs::write(&temp, &content).map_err(|e| LockError::Io {
            path: temp.display().to_string(),
            source: e,
        })?;
        std::fs::rename(&temp, path).map_err(|e| LockError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        Ok(())
    }

    // ── Queries ─────────────────────────────────────────────────

    /// Look up a locked package by name (PEP 503 normalized comparison).
    pub fn get_package(&self, name: &str) -> Option<&LockedPackage> {
        let normalized = normalize_pep503(name);
        self.packages
            .iter()
            .find(|p| normalize_pep503(&p.name) == normalized)
    }

    /// Whether this is a universal (cross-platform) lockfile.
    pub fn is_universal(&self) -> bool {
        self.options.resolution_markers.is_some()
    }

    /// Return the names of packages that should be installed for the given
    /// marker environment. Walks the dependency graph starting from all
    /// packages with no incoming marker constraints, following only
    /// dependencies whose markers match `env`.
    ///
    /// If this is not a universal lockfile, returns all package names.
    pub fn packages_for_environment(&self, evaluate_marker: &dyn Fn(&str) -> bool) -> Vec<String> {
        use std::collections::{HashSet, VecDeque};

        if !self.is_universal() {
            return self.packages.iter().map(|p| p.name.clone()).collect();
        }

        // Build an index of packages by normalized name.
        let pkg_by_name: std::collections::HashMap<String, &LockedPackage> = self
            .packages
            .iter()
            .map(|p| (normalize_pep503(&p.name), p))
            .collect();

        // Find root packages: those that are NOT referenced as a dependency
        // by any other package (or are referenced without a marker constraint).
        let mut referenced_with_marker_only: HashSet<String> = HashSet::new();
        let mut referenced_unconditionally: HashSet<String> = HashSet::new();

        for pkg in &self.packages {
            for dep in &pkg.dependencies {
                let norm = normalize_pep503(&dep.name);
                if dep.marker.is_some() {
                    referenced_with_marker_only.insert(norm);
                } else {
                    referenced_unconditionally.insert(norm.clone());
                }
            }
        }

        // Start BFS from all packages (we'll filter by marker during traversal).
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();

        // Seed: packages that are true roots — not referenced as a dependency
        // by any other package (neither with markers nor unconditionally).
        // In practice, the root project package is one of these.
        for pkg in &self.packages {
            let norm = normalize_pep503(&pkg.name);
            if !referenced_with_marker_only.contains(&norm)
                && !referenced_unconditionally.contains(&norm)
                && visited.insert(norm.clone())
            {
                queue.push_back(norm);
            }
        }

        // BFS: follow dependencies, respecting markers.
        while let Some(name) = queue.pop_front() {
            if let Some(pkg) = pkg_by_name.get(&name) {
                for dep in &pkg.dependencies {
                    // If the dependency has a marker, evaluate it.
                    if let Some(ref marker) = dep.marker {
                        if !evaluate_marker(marker) {
                            continue; // skip deps whose markers don't match
                        }
                    }
                    let dep_norm = normalize_pep503(&dep.name);
                    if visited.insert(dep_norm.clone()) {
                        queue.push_back(dep_norm);
                    }
                }
            }
        }

        visited.into_iter().collect()
    }

    // ── Staleness detection ─────────────────────────────────────

    /// Check whether this lockfile is stale relative to the current
    /// dependency strings and requires-python.
    ///
    /// Compares a hash of `current_deps` against a hash of the locked
    /// package set, and checks `requires_python` against `current_requires_python`.
    pub fn is_stale(&self, current_deps: &[String], current_requires_python: Option<&str>) -> bool {
        // Check requires-python mismatch
        match (&self.requires_python, current_requires_python) {
            (Some(locked), Some(current)) if locked != current => return true,
            (Some(_), None) | (None, Some(_)) => return true,
            _ => {}
        }

        // Compute a hash of the current dependency set and compare against
        // a hash of the locked state.
        let current_hash = compute_input_hash(current_deps);
        let stored_hash = self.input_hash();
        stored_hash != current_hash
    }

    /// Compute a hash representing the current state of locked packages.
    /// Note: requires_python is NOT included here because it is checked
    /// separately in `is_stale()`.
    fn input_hash(&self) -> String {
        let mut items: Vec<String> = self
            .packages
            .iter()
            .filter_map(|p| {
                p.version
                    .as_ref()
                    .map(|v| format!("{}=={}", normalize_pep503(&p.name), v))
            })
            .collect();
        items.sort();
        compute_input_hash_from_sorted(&items)
    }

    // ── Build from resolution ───────────────────────────────────

    /// Build a `UvLock` from a set of resolved packages.
    ///
    /// This is a simplified builder for v0.1.0 that creates
    /// packages. Packages and dependencies are sorted for determinism.
    pub fn from_resolution(packages: Vec<LockedPackage>, requires_python: Option<&str>) -> Self {
        let mut lock = UvLock {
            version: 1,
            revision: 3,
            requires_python: requires_python.map(|s| s.to_string()),
            options: LockOptions::default(),
            packages,
        };
        lock.packages.sort();
        for pkg in &mut lock.packages {
            pkg.dependencies.sort();
            for deps in pkg.optional_dependencies.values_mut() {
                deps.sort();
            }
            for deps in pkg.dev_dependencies.values_mut() {
                deps.sort();
            }
            pkg.wheels.sort();
        }
        lock
    }
}

impl fmt::Display for UvLock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.to_toml() {
            Ok(s) => f.write_str(&s),
            Err(e) => write!(f, "<!-- serialization error: {} -->", e),
        }
    }
}

// ── Parsing helpers ─────────────────────────────────────────────────

fn parse_options(item: Option<&Item>) -> LockOptions {
    match item {
        Some(Item::Table(t)) => {
            let resolution_markers = t.get("resolution-markers").and_then(|v| match v {
                Item::Value(Value::Array(arr)) => Some(
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect(),
                ),
                _ => None,
            });

            LockOptions {
                resolution_mode: t
                    .get("resolution-mode")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                prerelease_mode: t
                    .get("prerelease-mode")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                resolution_markers,
            }
        }
        _ => LockOptions::default(),
    }
}

fn parse_package(table: &Table) -> Result<LockedPackage, LockError> {
    let name = table
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| LockError::Invalid("package missing 'name' field".to_string()))?
        .to_string();

    let version = table
        .get("version")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let source = table
        .get("source")
        .map(parse_source)
        .transpose()?
        .unwrap_or_default();

    let dependencies = table
        .get("dependencies")
        .map(parse_dep_array)
        .transpose()?
        .unwrap_or_default();

    // Accept both "optional-dependencies" and check for it.
    // Also filter out spillover keys (sdist, wheels) that TOML places
    // inside the subtable when they appear after the [package.X] header.
    let optional_dependencies = table
        .get("optional-dependencies")
        .map(parse_dep_groups_filtered)
        .transpose()?
        .unwrap_or_default();

    // Accept both "dev-dependencies" and "dependency-groups"
    let dev_dependencies = table
        .get("dev-dependencies")
        .or_else(|| table.get("dependency-groups"))
        .map(parse_dep_groups_filtered)
        .transpose()?
        .unwrap_or_default();

    // Look for sdist/wheels at the package level first. If not found,
    // check inside subtables where TOML may have placed them if they
    // appear after a [package.optional-dependencies] header.
    let sdist = find_item_in_table_or_subtables(table, "sdist")
        .map(parse_artifact)
        .transpose()?;

    let wheels = find_item_in_table_or_subtables(table, "wheels")
        .map(parse_artifact_array)
        .transpose()?
        .unwrap_or_default();

    Ok(LockedPackage {
        name,
        version,
        source,
        dependencies,
        optional_dependencies,
        dev_dependencies,
        sdist,
        wheels,
    })
}

fn parse_source(item: &Item) -> Result<PackageSource, LockError> {
    let table = item_as_inline_or_table(item)
        .ok_or_else(|| LockError::Invalid("source must be a table".to_string()))?;

    if let Some(url) = get_str(&table, "registry") {
        Ok(PackageSource::Registry { url })
    } else if let Some(url) = get_str(&table, "git") {
        Ok(PackageSource::Git { url })
    } else if let Some(path) = get_str(&table, "editable") {
        Ok(PackageSource::Editable { path })
    } else if let Some(path) = get_str(&table, "virtual") {
        Ok(PackageSource::Virtual { path })
    } else if let Some(path) = get_str(&table, "directory") {
        Ok(PackageSource::Directory { path })
    } else if let Some(path) = get_str(&table, "path") {
        Ok(PackageSource::Path { path })
    } else {
        Err(LockError::Invalid(
            "unknown package source type".to_string(),
        ))
    }
}

/// Extract a string value from a key-value collection (works with both
/// inline tables and regular tables).
fn get_str(kv: &[(String, Value)], key: &str) -> Option<String> {
    kv.iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_str())
        .map(|s| s.to_string())
}

fn get_int(kv: &[(String, Value)], key: &str) -> Option<i64> {
    kv.iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_integer())
}

fn get_str_array(kv: &[(String, Value)], key: &str) -> Option<Vec<String>> {
    kv.iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
}

/// Convert an Item to a flat list of (key, value) pairs.
/// Works for both inline tables `{ foo = "bar" }` and regular `[table]`.
fn item_as_inline_or_table(item: &Item) -> Option<Vec<(String, Value)>> {
    match item {
        Item::Value(Value::InlineTable(it)) => {
            Some(it.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
        }
        Item::Table(t) => Some(
            t.iter()
                .filter_map(|(k, v)| {
                    if let Item::Value(val) = v {
                        Some((k.to_string(), val.clone()))
                    } else {
                        None
                    }
                })
                .collect(),
        ),
        _ => None,
    }
}

fn parse_dep_array(item: &Item) -> Result<Vec<Dependency>, LockError> {
    let arr = match item {
        Item::Value(Value::Array(a)) => a,
        _ => {
            return Err(LockError::Invalid(
                "dependencies must be an array".to_string(),
            ))
        }
    };

    arr.iter().map(parse_dep_value).collect()
}

fn parse_dep_value(val: &Value) -> Result<Dependency, LockError> {
    let it = val
        .as_inline_table()
        .ok_or_else(|| LockError::Invalid("dependency must be an inline table".to_string()))?;

    let kv: Vec<(String, Value)> = it.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();

    let name = get_str(&kv, "name")
        .ok_or_else(|| LockError::Invalid("dependency missing 'name'".to_string()))?;

    let version = get_str(&kv, "version");
    let marker = get_str(&kv, "marker");
    let extra = get_str_array(&kv, "extra");

    let source = kv
        .iter()
        .find(|(k, _)| k == "source")
        .map(|(_, v)| {
            let inner: Vec<(String, Value)> = v
                .as_inline_table()
                .map(|it| it.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
                .unwrap_or_default();
            parse_source_from_kv(&inner)
        })
        .transpose()?;

    Ok(Dependency {
        name,
        version,
        source,
        marker,
        extra,
    })
}

fn parse_source_from_kv(kv: &[(String, Value)]) -> Result<PackageSource, LockError> {
    if let Some(url) = get_str(kv, "registry") {
        Ok(PackageSource::Registry { url })
    } else if let Some(url) = get_str(kv, "git") {
        Ok(PackageSource::Git { url })
    } else if let Some(path) = get_str(kv, "editable") {
        Ok(PackageSource::Editable { path })
    } else if let Some(path) = get_str(kv, "virtual") {
        Ok(PackageSource::Virtual { path })
    } else if let Some(path) = get_str(kv, "directory") {
        Ok(PackageSource::Directory { path })
    } else if let Some(path) = get_str(kv, "path") {
        Ok(PackageSource::Path { path })
    } else {
        Err(LockError::Invalid(
            "unknown package source type".to_string(),
        ))
    }
}

/// Parse dependency groups, filtering out spillover keys (sdist, wheels, etc.)
/// that TOML places inside a subtable when they appear after its header.
fn parse_dep_groups_filtered(item: &Item) -> Result<BTreeMap<String, Vec<Dependency>>, LockError> {
    // Known keys that belong to the parent [[package]], not to dep groups.
    const SPILLOVER_KEYS: &[&str] = &[
        "sdist",
        "wheels",
        "name",
        "version",
        "source",
        "dependencies",
    ];

    let table = match item {
        Item::Table(t) => t,
        _ => {
            return Err(LockError::Invalid(
                "dependency groups must be a table".to_string(),
            ))
        }
    };

    let mut result = BTreeMap::new();
    for (key, value) in table.iter() {
        // Skip keys that are spillover from the parent package table.
        if SPILLOVER_KEYS.contains(&key) {
            continue;
        }
        let arr = match value {
            Item::Value(Value::Array(a)) => a,
            _ => continue,
        };
        let deps: Vec<Dependency> = arr.iter().map(parse_dep_value).collect::<Result<_, _>>()?;
        result.insert(key.to_string(), deps);
    }
    Ok(result)
}

/// Look for an item at the package table level first. If not found,
/// check inside known subtables where TOML may have placed it due to
/// key ordering (keys after a [package.X] header belong to that subtable).
fn find_item_in_table_or_subtables<'a>(table: &'a Table, key: &str) -> Option<&'a Item> {
    // Check at the top level first (correct case).
    if let Some(item) = table.get(key) {
        return Some(item);
    }

    // Check inside subtables (spillover case).
    for subtable_key in &[
        "optional-dependencies",
        "dev-dependencies",
        "dependency-groups",
    ] {
        if let Some(Item::Table(sub)) = table.get(subtable_key) {
            if let Some(item) = sub.get(key) {
                return Some(item);
            }
        }
    }

    None
}

fn parse_artifact(item: &Item) -> Result<Artifact, LockError> {
    let kv = item_as_inline_or_table(item)
        .ok_or_else(|| LockError::Invalid("artifact must be a table".to_string()))?;

    let hash = get_str(&kv, "hash")
        .ok_or_else(|| LockError::Invalid("artifact missing 'hash'".to_string()))?;

    Ok(Artifact {
        url: get_str(&kv, "url"),
        path: get_str(&kv, "path"),
        filename: get_str(&kv, "filename"),
        hash,
        size: get_int(&kv, "size").map(|i| i as u64),
    })
}

fn parse_artifact_array(item: &Item) -> Result<Vec<Artifact>, LockError> {
    let arr = match item {
        Item::Value(Value::Array(a)) => a,
        _ => return Err(LockError::Invalid("wheels must be an array".to_string())),
    };

    arr.iter()
        .map(|val| {
            let it = val
                .as_inline_table()
                .ok_or_else(|| LockError::Invalid("wheel must be an inline table".to_string()))?;
            let kv: Vec<(String, Value)> =
                it.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
            let hash = get_str(&kv, "hash")
                .ok_or_else(|| LockError::Invalid("artifact missing 'hash'".to_string()))?;
            Ok(Artifact {
                url: get_str(&kv, "url"),
                path: get_str(&kv, "path"),
                filename: get_str(&kv, "filename"),
                hash,
                size: get_int(&kv, "size").map(|i| i as u64),
            })
        })
        .collect()
}

// ── TOML string escaping ───────────────────────────────────────────

/// Escape a string for use inside a TOML quoted string (`"..."`).
///
/// Handles backslashes, double quotes, newlines, carriage returns, and tabs.
fn escape_toml_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}

// ── Formatting helpers ──────────────────────────────────────────────

/// Format a `PackageSource` as an inline TOML table.
fn format_source(source: &PackageSource) -> String {
    match source {
        PackageSource::Registry { url } => {
            format!("{{ registry = \"{}\" }}", escape_toml_string(url))
        }
        PackageSource::Git { url } => format!("{{ git = \"{}\" }}", escape_toml_string(url)),
        PackageSource::Path { path } => format!("{{ path = \"{}\" }}", escape_toml_string(path)),
        PackageSource::Directory { path } => {
            format!("{{ directory = \"{}\" }}", escape_toml_string(path))
        }
        PackageSource::Editable { path } => {
            format!("{{ editable = \"{}\" }}", escape_toml_string(path))
        }
        PackageSource::Virtual { path } => {
            format!("{{ virtual = \"{}\" }}", escape_toml_string(path))
        }
    }
}

/// Format a `Dependency` as an inline TOML table.
fn format_dependency(dep: &Dependency) -> String {
    let mut parts = vec![format!("name = \"{}\"", escape_toml_string(&dep.name))];
    if let Some(ref v) = dep.version {
        parts.push(format!("version = \"{}\"", escape_toml_string(v)));
    }
    if let Some(ref s) = dep.source {
        parts.push(format!("source = {}", format_source(s)));
    }
    if let Some(ref m) = dep.marker {
        parts.push(format!("marker = \"{}\"", escape_toml_string(m)));
    }
    if let Some(ref extras) = dep.extra {
        let extras_str: Vec<String> = extras
            .iter()
            .map(|e| format!("\"{}\"", escape_toml_string(e)))
            .collect();
        parts.push(format!("extra = [{}]", extras_str.join(", ")));
    }
    format!("{{ {} }}", parts.join(", "))
}

/// Format an `Artifact` as an inline TOML table.
fn format_artifact(artifact: &Artifact) -> String {
    let mut parts = Vec::new();
    if let Some(ref url) = artifact.url {
        parts.push(format!("url = \"{}\"", escape_toml_string(url)));
    }
    if let Some(ref path) = artifact.path {
        parts.push(format!("path = \"{}\"", escape_toml_string(path)));
    }
    if let Some(ref filename) = artifact.filename {
        parts.push(format!("filename = \"{}\"", escape_toml_string(filename)));
    }
    parts.push(format!("hash = \"{}\"", escape_toml_string(&artifact.hash)));
    if let Some(size) = artifact.size {
        parts.push(format!("size = {}", size));
    }
    format!("{{ {} }}", parts.join(", "))
}

// ── PEP 503 name normalization ──────────────────────────────────────

/// Normalize a package name per PEP 503: lowercase, replace runs of
/// `-`, `_`, or `.` with a single `-`.
pub fn normalize_pep503(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    let mut prev_was_sep = false;
    for c in name.chars() {
        if c == '-' || c == '_' || c == '.' {
            if !prev_was_sep {
                result.push('-');
            }
            prev_was_sep = true;
        } else {
            result.push(c.to_ascii_lowercase());
            prev_was_sep = false;
        }
    }
    result
}

// ── Input hash computation ──────────────────────────────────────────

/// Compute a SHA-256 hex digest of a sorted dependency list.
pub fn compute_input_hash(deps: &[String]) -> String {
    let mut sorted: Vec<&str> = deps.iter().map(|s| s.as_str()).collect();
    sorted.sort();
    let items: Vec<String> = sorted.iter().map(|s| s.to_string()).collect();
    compute_input_hash_from_sorted(&items)
}

fn compute_input_hash_from_sorted(items: &[String]) -> String {
    let mut hasher = Sha256::new();
    for item in items {
        hasher.update(item.as_bytes());
        hasher.update(b"\n");
    }
    hex::encode(hasher.finalize())
}

/// Compute a SHA-256 hex digest of a normalized, sorted dependency list
/// plus optional config inputs (Python version, index URL).
/// Used for backward-compatible staleness detection.
pub fn compute_input_hash_with_config(
    deps: &[String],
    python_version: Option<&str>,
    index_url: Option<&str>,
) -> String {
    let mut sorted: Vec<&str> = deps.iter().map(|s| s.as_str()).collect();
    sorted.sort();
    let mut hasher = Sha256::new();
    for dep in &sorted {
        hasher.update(dep.as_bytes());
        hasher.update(b"\n");
    }
    if let Some(pv) = python_version {
        hasher.update(b"python_version:");
        hasher.update(pv.as_bytes());
        hasher.update(b"\n");
    }
    if let Some(iu) = index_url {
        hasher.update(b"index_url:");
        hasher.update(iu.as_bytes());
        hasher.update(b"\n");
    }
    hex::encode(hasher.finalize())
}

// ── Backward-compatible Lockfile wrapper ────────────────────────────
// CLI commands currently reference `Lockfile`. Provide a migration path.

/// Backward-compatible wrapper around [`UvLock`].
///
/// This struct adapts the new `uv.lock` data model to the old `Lockfile` API
/// used by CLI commands (`from_path`, `packages`, `metadata.index_url`, etc.).
/// It will be removed once the CLI is fully migrated.
pub struct Lockfile {
    /// The underlying `UvLock` data model. Public for marker filtering in sync.
    pub inner: UvLock,
    pub metadata: LockfileMetadata,
    pub packages: Vec<FlatLockedPackage>,
}

/// Backward-compatible metadata (matches old API).
pub struct LockfileMetadata {
    pub umbral_version: String,
    pub input_hash: String,
    pub python_version: Option<String>,
    pub index_url: Option<String>,
}

/// A flattened locked package for backward compatibility with the old API.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FlatLockedPackage {
    pub name: String,
    pub version: String,
    pub source: String,
    pub dependencies: Vec<String>,
    pub hashes: Vec<String>,
    pub requires_python: Option<String>,
    pub markers: Option<String>,
    /// Wheel artifacts with URLs and filenames for lockfile generation.
    pub wheel_artifacts: Vec<FlatArtifact>,
}

/// A simplified artifact for passing through the lockfile pipeline.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FlatArtifact {
    pub url: String,
    pub filename: String,
    pub hash: Option<String>,
}

impl FromStr for Lockfile {
    type Err = LockError;

    fn from_str(content: &str) -> Result<Self, Self::Err> {
        let inner = UvLock::from_str(content)?;
        let packages = inner
            .packages
            .iter()
            .map(|p| {
                let source_str = match &p.source {
                    PackageSource::Registry { url } => url.clone(),
                    PackageSource::Git { url } => url.clone(),
                    PackageSource::Path { path } => path.clone(),
                    PackageSource::Directory { path } => path.clone(),
                    PackageSource::Editable { path } => path.clone(),
                    PackageSource::Virtual { path } => path.clone(),
                };
                FlatLockedPackage {
                    name: p.name.clone(),
                    version: p.version.clone().unwrap_or_default(),
                    source: source_str,
                    dependencies: p.dependencies.iter().map(|d| d.name.clone()).collect(),
                    hashes: p.wheels.iter().map(|w| w.hash.clone()).collect(),
                    requires_python: None,
                    markers: None,
                    wheel_artifacts: p
                        .wheels
                        .iter()
                        .filter(|w| w.url.is_some() && w.filename.is_some())
                        .map(|w| FlatArtifact {
                            url: w.url.clone().unwrap_or_default(),
                            filename: w.filename.clone().unwrap_or_default(),
                            hash: Some(w.hash.clone()),
                        })
                        .collect(),
                }
            })
            .collect();

        Ok(Lockfile {
            metadata: LockfileMetadata {
                umbral_version: "0.1.0".to_string(),
                input_hash: String::new(),
                python_version: inner.requires_python.clone(),
                index_url: None,
            },
            packages,
            inner,
        })
    }
}

impl Lockfile {
    pub fn new(
        umbral_version: impl Into<String>,
        dependency_strings: &[String],
        packages: Vec<FlatLockedPackage>,
        python_version: Option<&str>,
        index_url: Option<&str>,
    ) -> Self {
        let input_hash =
            compute_input_hash_with_config(dependency_strings, python_version, index_url);

        let uv_packages: Vec<LockedPackage> = packages
            .iter()
            .map(|p| LockedPackage {
                name: p.name.clone(),
                version: Some(p.version.clone()),
                source: PackageSource::Registry {
                    url: p.source.clone(),
                },
                dependencies: p
                    .dependencies
                    .iter()
                    .map(|d| Dependency {
                        name: d.clone(),
                        version: None,
                        source: None,
                        marker: None,
                        extra: None,
                    })
                    .collect(),
                optional_dependencies: BTreeMap::new(),
                dev_dependencies: BTreeMap::new(),
                sdist: None,
                wheels: if !p.wheel_artifacts.is_empty() {
                    p.wheel_artifacts
                        .iter()
                        .map(|a| Artifact {
                            url: Some(a.url.clone()),
                            path: None,
                            filename: Some(a.filename.clone()),
                            hash: a
                                .hash
                                .clone()
                                .unwrap_or_else(|| "sha256:unknown".to_string()),
                            size: None,
                        })
                        .collect()
                } else {
                    p.hashes
                        .iter()
                        .map(|h| Artifact {
                            url: None,
                            path: None,
                            filename: None,
                            hash: h.clone(),
                            size: None,
                        })
                        .collect()
                },
            })
            .collect();

        let inner = UvLock::from_resolution(uv_packages, python_version);

        // Re-sort the flat packages too.
        let mut sorted_packages = packages;
        sorted_packages.sort();
        for p in &mut sorted_packages {
            p.dependencies.sort();
            p.hashes.sort();
        }

        Lockfile {
            inner,
            metadata: LockfileMetadata {
                umbral_version: umbral_version.into(),
                input_hash,
                python_version: python_version.map(|s| s.to_string()),
                index_url: index_url.map(|s| s.to_string()),
            },
            packages: sorted_packages,
        }
    }

    pub fn to_toml(&self) -> Result<String, LockError> {
        self.inner.to_toml()
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(content: &str) -> Result<Self, LockError> {
        <Self as FromStr>::from_str(content)
    }

    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, LockError> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path).map_err(|e| LockError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        Self::from_str(&content)
    }

    pub fn write_to(&self, path: impl AsRef<Path>) -> Result<(), LockError> {
        self.inner.write_to(path)
    }

    pub fn is_stale(
        &self,
        current_deps: &[String],
        python_version: Option<&str>,
        index_url: Option<&str>,
    ) -> bool {
        let current_hash = compute_input_hash_with_config(current_deps, python_version, index_url);
        self.metadata.input_hash != current_hash
    }

    pub fn get_package(&self, name: &str) -> Option<&FlatLockedPackage> {
        let normalized = normalize_pep503(name);
        self.packages
            .iter()
            .find(|p| normalize_pep503(&p.name) == normalized)
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Sample uv.lock content ──────────────────────────────────

    fn sample_uv_lock() -> &'static str {
        r#"# This file was @generated by uv. Do not edit manually.
version = 1
revision = 3
requires-python = ">=3.12"

[[package]]
name = "certifi"
version = "2024.2.2"
source = { registry = "https://pypi.org/simple" }
wheels = [
    { url = "https://files.pythonhosted.org/certifi-2024.2.2-py3-none-any.whl", hash = "sha256:dc383c07b76109f368f6106eee2b593b04a011ea4d55f652c6ca24a754d1cdd1", size = 163774 },
]

[[package]]
name = "charset-normalizer"
version = "3.3.2"
source = { registry = "https://pypi.org/simple" }
wheels = [
    { url = "https://files.pythonhosted.org/charset_normalizer-3.3.2-py3-none-any.whl", hash = "sha256:3e4d1f6587322d2788836a99c69062fbb091331ec940e02d12d179c1d53e25fc", size = 48561 },
]

[[package]]
name = "idna"
version = "3.7"
source = { registry = "https://pypi.org/simple" }
wheels = [
    { url = "https://files.pythonhosted.org/idna-3.7-py3-none-any.whl", hash = "sha256:82fee1fc78add43492d3a1898bfa6d8a904cc97d8427f683ed8e798d07761aa0", size = 66836 },
]

[[package]]
name = "requests"
version = "2.32.4"
source = { registry = "https://pypi.org/simple" }
dependencies = [
    { name = "certifi" },
    { name = "charset-normalizer" },
    { name = "idna" },
    { name = "urllib3" },
]
sdist = { url = "https://files.pythonhosted.org/requests-2.32.4.tar.gz", hash = "sha256:e3c35a1a3cb644a498eb7c44a46b76af9a358ef5f5e025e24e921adea7762525", size = 131234 }
wheels = [
    { url = "https://files.pythonhosted.org/requests-2.32.4-py3-none-any.whl", hash = "sha256:344d4f2e05f4ed8bce0c88bce9f9a0e4defc82be1d7d0a1a6083c8e3a44e2e36", size = 63456 },
]

[package.optional-dependencies]
security = [
    { name = "pyopenssl" },
]

[[package]]
name = "urllib3"
version = "2.2.1"
source = { registry = "https://pypi.org/simple" }
wheels = [
    { url = "https://files.pythonhosted.org/urllib3-2.2.1-py3-none-any.whl", hash = "sha256:450b20ec296a467077128bff42b73080516e71b56ff59a60a02bef2232c4fa9d", size = 121060 },
]
"#
    }

    // ── Test: Parse a realistic uv.lock ─────────────────────────

    #[test]
    fn parse_realistic_uv_lock() {
        let lock = UvLock::from_str(sample_uv_lock()).unwrap();

        assert_eq!(lock.version, 1);
        assert_eq!(lock.revision, 3);
        assert_eq!(lock.requires_python.as_deref(), Some(">=3.12"));
        assert_eq!(lock.packages.len(), 5);

        // Check requests package
        let requests = lock.get_package("requests").unwrap();
        assert_eq!(requests.version.as_deref(), Some("2.32.4"));
        assert_eq!(requests.dependencies.len(), 4);
        assert!(requests.sdist.is_some());
        assert_eq!(requests.wheels.len(), 1);

        // Check optional deps
        assert!(requests.optional_dependencies.contains_key("security"));
        let security_deps = &requests.optional_dependencies["security"];
        assert_eq!(security_deps.len(), 1);
        assert_eq!(security_deps[0].name, "pyopenssl");

        // Check certifi (leaf package)
        let certifi = lock.get_package("certifi").unwrap();
        assert_eq!(certifi.version.as_deref(), Some("2024.2.2"));
        assert!(certifi.dependencies.is_empty());
        assert_eq!(certifi.wheels.len(), 1);
        assert_eq!(certifi.wheels[0].size, Some(163774));
    }

    // ── Test: Round-trip write then parse ────────────────────────

    #[test]
    fn round_trip_write_then_parse() {
        let original = UvLock::from_str(sample_uv_lock()).unwrap();
        let toml_str = original.to_toml().unwrap();
        let reparsed = UvLock::from_str(&toml_str).unwrap();

        assert_eq!(original.version, reparsed.version);
        assert_eq!(original.revision, reparsed.revision);
        assert_eq!(original.requires_python, reparsed.requires_python);
        assert_eq!(original.packages.len(), reparsed.packages.len());

        for orig_pkg in &original.packages {
            let reparsed_pkg = reparsed.get_package(&orig_pkg.name).unwrap();
            assert_eq!(orig_pkg.name, reparsed_pkg.name);
            assert_eq!(orig_pkg.version, reparsed_pkg.version);
            assert_eq!(orig_pkg.source, reparsed_pkg.source);
            assert_eq!(orig_pkg.dependencies.len(), reparsed_pkg.dependencies.len());
            assert_eq!(
                orig_pkg.optional_dependencies,
                reparsed_pkg.optional_dependencies
            );
            assert_eq!(orig_pkg.sdist, reparsed_pkg.sdist);
            assert_eq!(orig_pkg.wheels.len(), reparsed_pkg.wheels.len());
        }
    }

    // ── Test: Deterministic output ──────────────────────────────

    #[test]
    fn deterministic_output() {
        let lock = UvLock::from_str(sample_uv_lock()).unwrap();
        let out1 = lock.to_toml().unwrap();
        let out2 = lock.to_toml().unwrap();
        assert_eq!(
            out1, out2,
            "serializing twice must produce identical output"
        );
    }

    // ── Test: Packages sorted by name ───────────────────────────

    #[test]
    fn packages_sorted_by_name_in_output() {
        let lock = UvLock::from_str(sample_uv_lock()).unwrap();
        let toml_str = lock.to_toml().unwrap();

        let names: Vec<&str> = toml_str
            .lines()
            .filter(|l| l.starts_with("name = "))
            .map(|l| l.trim_start_matches("name = \"").trim_end_matches('"'))
            .collect();

        let mut sorted_names = names.clone();
        sorted_names.sort();
        assert_eq!(names, sorted_names, "packages must appear in sorted order");
    }

    // ── Test: Header comment present ────────────────────────────

    #[test]
    fn header_comment_present() {
        let lock = UvLock::from_str(sample_uv_lock()).unwrap();
        let toml_str = lock.to_toml().unwrap();
        assert!(
            toml_str.starts_with("# This file was @generated"),
            "output should start with @generated header"
        );
    }

    // ── Test: get_package with PEP 503 normalization ────────────

    #[test]
    fn get_package_normalized() {
        let lock = UvLock::from_str(sample_uv_lock()).unwrap();

        assert!(lock.get_package("certifi").is_some());
        assert!(lock.get_package("Certifi").is_some());
        assert!(lock.get_package("CERTIFI").is_some());
        assert!(lock.get_package("charset-normalizer").is_some());
        assert!(lock.get_package("charset_normalizer").is_some());
        assert!(lock.get_package("Charset.Normalizer").is_some());
        assert!(lock.get_package("nonexistent").is_none());
    }

    // ── Test: Parse with optional-dependencies and dev-dependencies ─

    #[test]
    fn parse_optional_and_dev_dependencies() {
        let content = r#"
version = 1
revision = 3
requires-python = ">=3.12"

[[package]]
name = "my-project"
version = "0.1.0"
source = { virtual = "." }
dependencies = [
    { name = "requests" },
]

[package.optional-dependencies]
security = [
    { name = "pyopenssl" },
]
socks = [
    { name = "pysocks" },
]

[package.dev-dependencies]
dev = [
    { name = "pytest" },
    { name = "ruff" },
]

[[package]]
name = "requests"
version = "2.32.4"
source = { registry = "https://pypi.org/simple" }
wheels = [
    { hash = "sha256:abcdef" },
]
"#;

        let lock = UvLock::from_str(content).unwrap();

        let project = lock.get_package("my-project").unwrap();
        assert_eq!(project.optional_dependencies.len(), 2);
        assert!(project.optional_dependencies.contains_key("security"));
        assert!(project.optional_dependencies.contains_key("socks"));
        assert_eq!(project.dev_dependencies.len(), 1);
        assert!(project.dev_dependencies.contains_key("dev"));
        assert_eq!(project.dev_dependencies["dev"].len(), 2);
    }

    // ── Test: Parse with `dependency-groups` alias ───────────────

    #[test]
    fn parse_dependency_groups_alias() {
        let content = r#"
version = 1
revision = 3

[[package]]
name = "my-app"
version = "0.1.0"
source = { virtual = "." }

[package.dependency-groups]
dev = [
    { name = "pytest" },
]
"#;

        let lock = UvLock::from_str(content).unwrap();
        let app = lock.get_package("my-app").unwrap();
        assert_eq!(app.dev_dependencies.len(), 1);
        assert_eq!(app.dev_dependencies["dev"][0].name, "pytest");
    }

    // ── Test: Parse with `distribution` alias ───────────────────

    #[test]
    fn parse_distribution_alias() {
        let content = r#"
version = 1
revision = 2

[[distribution]]
name = "foo"
version = "1.0.0"
source = { registry = "https://pypi.org/simple" }
wheels = [
    { hash = "sha256:abc123" },
]
"#;

        let lock = UvLock::from_str(content).unwrap();
        assert_eq!(lock.packages.len(), 1);
        assert_eq!(lock.packages[0].name, "foo");
    }

    // ── Test: Handle missing optional fields gracefully ─────────

    #[test]
    fn parse_minimal_package() {
        let content = r#"
version = 1

[[package]]
name = "bare-minimum"
source = { registry = "https://pypi.org/simple" }
"#;

        let lock = UvLock::from_str(content).unwrap();
        assert_eq!(lock.packages.len(), 1);
        let pkg = &lock.packages[0];
        assert_eq!(pkg.name, "bare-minimum");
        assert_eq!(pkg.version, None);
        assert!(pkg.dependencies.is_empty());
        assert!(pkg.optional_dependencies.is_empty());
        assert!(pkg.dev_dependencies.is_empty());
        assert!(pkg.sdist.is_none());
        assert!(pkg.wheels.is_empty());
        assert_eq!(lock.requires_python, None);
    }

    // ── Test: Staleness detection ───────────────────────────────

    #[test]
    fn staleness_detection_requires_python_changed() {
        let lock = UvLock {
            version: 1,
            revision: 3,
            requires_python: Some(">=3.12".to_string()),
            options: LockOptions::default(),
            packages: vec![],
        };

        assert!(!lock.is_stale(&[], Some(">=3.12")));
        assert!(lock.is_stale(&[], Some(">=3.11")));
        assert!(lock.is_stale(&[], None));
    }

    #[test]
    fn staleness_detection_no_requires_python() {
        let lock = UvLock {
            version: 1,
            revision: 3,
            requires_python: None,
            options: LockOptions::default(),
            packages: vec![],
        };

        assert!(!lock.is_stale(&[], None));
        assert!(lock.is_stale(&[], Some(">=3.12")));
    }

    // ── Test: Various PackageSource types ───────────────────────

    #[test]
    fn parse_various_source_types() {
        let content = r#"
version = 1

[[package]]
name = "from-git"
version = "1.0.0"
source = { git = "https://github.com/user/repo.git?rev=abc123" }
wheels = [
    { hash = "sha256:aaa" },
]

[[package]]
name = "from-path"
version = "0.1.0"
source = { path = "./vendored/from-path-0.1.0.tar.gz" }
wheels = [
    { hash = "sha256:bbb" },
]

[[package]]
name = "from-dir"
version = "0.2.0"
source = { directory = "./libs/from-dir" }
wheels = [
    { hash = "sha256:ccc" },
]

[[package]]
name = "editable-pkg"
version = "0.3.0"
source = { editable = "./packages/editable-pkg" }
wheels = [
    { hash = "sha256:ddd" },
]

[[package]]
name = "virtual-root"
version = "0.1.0"
source = { virtual = "." }
"#;

        let lock = UvLock::from_str(content).unwrap();
        assert_eq!(lock.packages.len(), 5);

        assert!(matches!(
            &lock.get_package("from-git").unwrap().source,
            PackageSource::Git { url } if url.contains("github.com")
        ));
        assert!(matches!(
            &lock.get_package("from-path").unwrap().source,
            PackageSource::Path { path } if path.contains("vendored")
        ));
        assert!(matches!(
            &lock.get_package("from-dir").unwrap().source,
            PackageSource::Directory { path } if path.contains("libs")
        ));
        assert!(matches!(
            &lock.get_package("editable-pkg").unwrap().source,
            PackageSource::Editable { path } if path.contains("packages")
        ));
        assert!(matches!(
            &lock.get_package("virtual-root").unwrap().source,
            PackageSource::Virtual { path } if path == "."
        ));
    }

    // ── Test: Dependencies with markers, versions, extras ───────

    #[test]
    fn parse_dependencies_with_markers_and_extras() {
        let content = r#"
version = 1

[[package]]
name = "my-pkg"
version = "1.0.0"
source = { registry = "https://pypi.org/simple" }
dependencies = [
    { name = "colorama", marker = "sys_platform == 'win32'" },
    { name = "typing-extensions", version = "4.9.0" },
    { name = "requests", extra = ["security"] },
]
wheels = [
    { hash = "sha256:abc" },
]
"#;

        let lock = UvLock::from_str(content).unwrap();
        let pkg = lock.get_package("my-pkg").unwrap();
        assert_eq!(pkg.dependencies.len(), 3);

        let colorama = pkg
            .dependencies
            .iter()
            .find(|d| d.name == "colorama")
            .unwrap();
        assert_eq!(colorama.marker.as_deref(), Some("sys_platform == 'win32'"));

        let typing_ext = pkg
            .dependencies
            .iter()
            .find(|d| d.name == "typing-extensions")
            .unwrap();
        assert_eq!(typing_ext.version.as_deref(), Some("4.9.0"));

        let requests = pkg
            .dependencies
            .iter()
            .find(|d| d.name == "requests")
            .unwrap();
        assert_eq!(
            requests.extra.as_deref(),
            Some(&["security".to_string()][..])
        );
    }

    // ── Test: Artifact with various fields ──────────────────────

    #[test]
    fn parse_artifact_fields() {
        let content = r#"
version = 1

[[package]]
name = "pkg"
version = "1.0.0"
source = { registry = "https://pypi.org/simple" }

sdist = { url = "https://example.com/pkg-1.0.0.tar.gz", hash = "sha256:abc", size = 12345 }

wheels = [
    { url = "https://example.com/pkg-1.0.0-py3-none-any.whl", hash = "sha256:def", size = 6789 },
    { filename = "pkg-1.0.0-cp312-cp312-manylinux_2_17_x86_64.whl", hash = "sha256:ghi" },
    { path = "./wheels/pkg-1.0.0-py3-none-any.whl", hash = "sha256:jkl", size = 4321 },
]
"#;

        let lock = UvLock::from_str(content).unwrap();
        let pkg = lock.get_package("pkg").unwrap();

        let sdist = pkg.sdist.as_ref().unwrap();
        assert_eq!(
            sdist.url.as_deref(),
            Some("https://example.com/pkg-1.0.0.tar.gz")
        );
        assert_eq!(sdist.hash, "sha256:abc");
        assert_eq!(sdist.size, Some(12345));

        assert_eq!(pkg.wheels.len(), 3);
        assert!(pkg.wheels[0].url.is_some());
        assert!(pkg.wheels[1].filename.is_some());
        assert!(pkg.wheels[2].path.is_some());
    }

    // ── Test: Write and read file (atomic) ──────────────────────

    #[test]
    fn write_and_read_file() {
        let lock = UvLock::from_str(sample_uv_lock()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("uv.lock");

        lock.write_to(&path).unwrap();
        let loaded = UvLock::from_file(&path).unwrap();

        assert_eq!(lock.version, loaded.version);
        assert_eq!(lock.revision, loaded.revision);
        assert_eq!(lock.requires_python, loaded.requires_python);
        assert_eq!(lock.packages.len(), loaded.packages.len());
    }

    // ── Test: from_resolution builder ───────────────────────────

    #[test]
    fn from_resolution_sorts_and_normalizes() {
        let packages = vec![
            LockedPackage {
                name: "zlib".to_string(),
                version: Some("1.0.0".to_string()),
                source: PackageSource::Registry {
                    url: "https://pypi.org/simple".to_string(),
                },
                dependencies: vec![
                    Dependency {
                        name: "bbb".to_string(),
                        version: None,
                        source: None,
                        marker: None,
                        extra: None,
                    },
                    Dependency {
                        name: "aaa".to_string(),
                        version: None,
                        source: None,
                        marker: None,
                        extra: None,
                    },
                ],
                optional_dependencies: BTreeMap::new(),
                dev_dependencies: BTreeMap::new(),
                sdist: None,
                wheels: vec![],
            },
            LockedPackage {
                name: "aaa".to_string(),
                version: Some("2.0.0".to_string()),
                source: PackageSource::Registry {
                    url: "https://pypi.org/simple".to_string(),
                },
                dependencies: vec![],
                optional_dependencies: BTreeMap::new(),
                dev_dependencies: BTreeMap::new(),
                sdist: None,
                wheels: vec![],
            },
        ];

        let lock = UvLock::from_resolution(packages, Some(">=3.12"));

        assert_eq!(lock.packages[0].name, "aaa");
        assert_eq!(lock.packages[1].name, "zlib");

        let zlib_deps: Vec<&str> = lock.packages[1]
            .dependencies
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        assert_eq!(zlib_deps, vec!["aaa", "bbb"]);
    }

    // ── Test: Options section ───────────────────────────────────

    #[test]
    fn parse_and_write_options() {
        let content = r#"
version = 1
revision = 3

[options]
resolution-mode = "lowest"
prerelease-mode = "allow"

[[package]]
name = "foo"
version = "1.0.0"
source = { registry = "https://pypi.org/simple" }
wheels = [
    { hash = "sha256:abc" },
]
"#;

        let lock = UvLock::from_str(content).unwrap();
        assert_eq!(lock.options.resolution_mode.as_deref(), Some("lowest"));
        assert_eq!(lock.options.prerelease_mode.as_deref(), Some("allow"));

        let toml_str = lock.to_toml().unwrap();
        assert!(toml_str.contains("resolution-mode = \"lowest\""));
        assert!(toml_str.contains("prerelease-mode = \"allow\""));
    }

    // ── Test: Options omitted when empty ────────────────────────

    #[test]
    fn options_omitted_when_empty() {
        let lock = UvLock {
            version: 1,
            revision: 3,
            requires_python: Some(">=3.12".to_string()),
            options: LockOptions::default(),
            packages: vec![],
        };

        let toml_str = lock.to_toml().unwrap();
        assert!(
            !toml_str.contains("[options]"),
            "empty options should not appear in output"
        );
    }

    // ── Test: Backward-compatible Lockfile wrapper ──────────────

    #[test]
    fn backward_compat_lockfile_new_and_get_package() {
        let packages = vec![
            FlatLockedPackage {
                name: "requests".into(),
                version: "2.31.0".into(),
                source: "https://pypi.org/simple/requests/".into(),
                dependencies: vec!["urllib3".into(), "certifi".into()],
                hashes: vec!["sha256:abcdef".into()],
                requires_python: Some(">=3.7".into()),
                markers: None,
                wheel_artifacts: vec![],
            },
            FlatLockedPackage {
                name: "certifi".into(),
                version: "2024.2.2".into(),
                source: "https://pypi.org/simple/certifi/".into(),
                dependencies: vec![],
                hashes: vec![],
                requires_python: None,
                markers: None,
                wheel_artifacts: vec![],
            },
        ];
        let deps = vec!["requests>=2.28".to_string()];
        let lf = Lockfile::new("0.1.0", &deps, packages, None, None);

        assert_eq!(lf.packages.len(), 2);
        let req = lf.get_package("requests").unwrap();
        assert_eq!(req.version, "2.31.0");

        let req2 = lf.get_package("Requests").unwrap();
        assert_eq!(req2.version, "2.31.0");
    }

    #[test]
    fn backward_compat_staleness() {
        let deps = vec!["requests>=2.28".to_string(), "click>=8.0".to_string()];
        let lf = Lockfile::new("0.1.0", &deps, vec![], None, None);

        assert!(!lf.is_stale(&deps, None, None));

        let new_deps = vec!["requests>=2.31".to_string(), "click>=8.0".to_string()];
        assert!(lf.is_stale(&new_deps, None, None));

        let reversed = vec!["click>=8.0".to_string(), "requests>=2.28".to_string()];
        assert!(!lf.is_stale(&reversed, None, None));
    }

    #[test]
    fn backward_compat_staleness_python_version() {
        let deps = vec!["requests>=2.28".to_string()];
        let lf = Lockfile::new("0.1.0", &deps, vec![], Some("3.11"), None);

        assert!(!lf.is_stale(&deps, Some("3.11"), None));
        assert!(lf.is_stale(&deps, Some("3.12"), None));
        assert!(lf.is_stale(&deps, None, None));
    }

    #[test]
    fn backward_compat_staleness_index_url() {
        let deps = vec!["requests>=2.28".to_string()];
        let lf = Lockfile::new(
            "0.1.0",
            &deps,
            vec![],
            None,
            Some("https://pypi.org/simple/"),
        );

        assert!(!lf.is_stale(&deps, None, Some("https://pypi.org/simple/")));
        assert!(lf.is_stale(&deps, None, Some("https://test.pypi.org/simple/")));
    }

    // ── Test: Skip unknown fields gracefully ────────────────────

    #[test]
    fn skip_unknown_fields() {
        let content = r#"
version = 1
revision = 3
requires-python = ">=3.12"
some-future-field = "hello"

[options]
resolution-mode = "lowest"
future-option = true

[[package]]
name = "foo"
version = "1.0.0"
source = { registry = "https://pypi.org/simple" }
future-package-field = 42
dependencies = [
    { name = "bar", future-dep-field = true },
]
wheels = [
    { hash = "sha256:abc", future-wheel-field = "xyz" },
]
"#;

        let lock = UvLock::from_str(content).unwrap();
        assert_eq!(lock.packages.len(), 1);
        assert_eq!(lock.packages[0].name, "foo");
    }

    // ── Test: PEP 503 normalization function ────────────────────

    #[test]
    fn pep503_normalization() {
        assert_eq!(normalize_pep503("My.Package"), "my-package");
        assert_eq!(normalize_pep503("my_package"), "my-package");
        assert_eq!(normalize_pep503("my-package"), "my-package");
        assert_eq!(normalize_pep503("MY--PACKAGE"), "my-package");
        assert_eq!(normalize_pep503("my_.package"), "my-package");
        assert_eq!(normalize_pep503("simple"), "simple");
    }

    // ── Test: Display impl ──────────────────────────────────────

    #[test]
    fn display_impl() {
        let lock = UvLock::from_str(sample_uv_lock()).unwrap();
        let display_str = format!("{}", lock);
        assert!(display_str.starts_with("# This file was @generated"));
        assert!(display_str.contains("[[package]]"));
    }

    // ── Test: Empty lockfile ────────────────────────────────────

    #[test]
    fn empty_lockfile() {
        let content = "version = 1\nrevision = 3\n";
        let lock = UvLock::from_str(content).unwrap();
        assert_eq!(lock.version, 1);
        assert_eq!(lock.revision, 3);
        assert!(lock.packages.is_empty());
        assert!(lock.requires_python.is_none());
    }

    // ── Test: escape_toml_string ───────────────────────────────

    #[test]
    fn escape_toml_string_basic() {
        assert_eq!(escape_toml_string("hello"), "hello");
        assert_eq!(escape_toml_string(r#"say "hi""#), r#"say \"hi\""#);
        assert_eq!(escape_toml_string("back\\slash"), "back\\\\slash");
        assert_eq!(escape_toml_string("line\nbreak"), "line\\nbreak");
        assert_eq!(escape_toml_string("cr\rreturn"), "cr\\rreturn");
        assert_eq!(escape_toml_string("a\ttab"), "a\\ttab");
    }

    // ── Tests: Round-trip with special characters ──────────────

    #[test]
    fn round_trip_package_name_with_double_quote() {
        let lock = UvLock {
            version: 1,
            revision: 1,
            requires_python: None,
            options: LockOptions::default(),
            packages: vec![LockedPackage {
                name: "foo\"bar".to_string(),
                version: Some("1.0.0".to_string()),
                source: PackageSource::Registry {
                    url: "https://pypi.org/simple".to_string(),
                },
                dependencies: vec![],
                optional_dependencies: BTreeMap::new(),
                dev_dependencies: BTreeMap::new(),
                sdist: None,
                wheels: vec![Artifact {
                    url: None,
                    path: None,
                    filename: None,
                    hash: "sha256:aaa".to_string(),
                    size: None,
                }],
            }],
        };

        let toml_str = lock.to_toml().unwrap();
        let reparsed = UvLock::from_str(&toml_str).unwrap();
        assert_eq!(reparsed.packages[0].name, "foo\"bar");
    }

    #[test]
    fn round_trip_path_source_with_backslashes() {
        let lock = UvLock {
            version: 1,
            revision: 1,
            requires_python: None,
            options: LockOptions::default(),
            packages: vec![LockedPackage {
                name: "mypkg".to_string(),
                version: Some("1.0.0".to_string()),
                source: PackageSource::Path {
                    path: "C:\\Users\\test\\packages".to_string(),
                },
                dependencies: vec![],
                optional_dependencies: BTreeMap::new(),
                dev_dependencies: BTreeMap::new(),
                sdist: None,
                wheels: vec![Artifact {
                    url: None,
                    path: None,
                    filename: None,
                    hash: "sha256:bbb".to_string(),
                    size: None,
                }],
            }],
        };

        let toml_str = lock.to_toml().unwrap();
        let reparsed = UvLock::from_str(&toml_str).unwrap();
        assert_eq!(
            reparsed.packages[0].source,
            PackageSource::Path {
                path: "C:\\Users\\test\\packages".to_string()
            }
        );
    }

    #[test]
    fn round_trip_url_with_special_characters() {
        let special_url = "https://example.com/path?q=\"test\"&x=1".to_string();
        let lock = UvLock {
            version: 1,
            revision: 1,
            requires_python: None,
            options: LockOptions::default(),
            packages: vec![LockedPackage {
                name: "mypkg".to_string(),
                version: Some("1.0.0".to_string()),
                source: PackageSource::Registry {
                    url: special_url.clone(),
                },
                dependencies: vec![],
                optional_dependencies: BTreeMap::new(),
                dev_dependencies: BTreeMap::new(),
                sdist: None,
                wheels: vec![Artifact {
                    url: Some(special_url.clone()),
                    path: None,
                    filename: None,
                    hash: "sha256:ccc".to_string(),
                    size: None,
                }],
            }],
        };

        let toml_str = lock.to_toml().unwrap();
        let reparsed = UvLock::from_str(&toml_str).unwrap();
        assert_eq!(
            reparsed.packages[0].source,
            PackageSource::Registry {
                url: special_url.clone()
            }
        );
        assert_eq!(
            reparsed.packages[0].wheels[0].url.as_deref(),
            Some(special_url.as_str())
        );
    }

    #[test]
    fn round_trip_marker_with_quotes() {
        let marker = "python_version >= \"3.8\"".to_string();
        let lock = UvLock {
            version: 1,
            revision: 1,
            requires_python: None,
            options: LockOptions::default(),
            packages: vec![LockedPackage {
                name: "parent".to_string(),
                version: Some("1.0.0".to_string()),
                source: PackageSource::Registry {
                    url: "https://pypi.org/simple".to_string(),
                },
                dependencies: vec![Dependency {
                    name: "child".to_string(),
                    version: None,
                    source: None,
                    marker: Some(marker.clone()),
                    extra: None,
                }],
                optional_dependencies: BTreeMap::new(),
                dev_dependencies: BTreeMap::new(),
                sdist: None,
                wheels: vec![Artifact {
                    url: None,
                    path: None,
                    filename: None,
                    hash: "sha256:ddd".to_string(),
                    size: None,
                }],
            }],
        };

        let toml_str = lock.to_toml().unwrap();
        let reparsed = UvLock::from_str(&toml_str).unwrap();
        assert_eq!(
            reparsed.packages[0].dependencies[0].marker.as_deref(),
            Some(marker.as_str())
        );
    }

    // ── Edge case: Parse a realistic large lockfile with 10+ packages ─

    #[test]
    fn parse_large_lockfile_with_varied_sources() {
        let content = r#"
version = 1
revision = 3
requires-python = ">=3.10"

[options]
resolution-mode = "highest"
prerelease-mode = "disallow"

[[package]]
name = "my-project"
version = "0.1.0"
source = { virtual = "." }
dependencies = [
    { name = "requests", extra = ["security"] },
    { name = "click" },
    { name = "my-lib" },
]

[package.optional-dependencies]
dev = [
    { name = "pytest" },
    { name = "ruff" },
]
docs = [
    { name = "sphinx" },
]

[package.dev-dependencies]
test = [
    { name = "pytest" },
    { name = "coverage" },
]

[[package]]
name = "requests"
version = "2.31.0"
source = { registry = "https://pypi.org/simple" }
dependencies = [
    { name = "urllib3", version = "2.1.0" },
    { name = "certifi" },
    { name = "charset-normalizer" },
    { name = "idna" },
    { name = "colorama", marker = "sys_platform == 'win32'" },
]
sdist = { url = "https://files.pythonhosted.org/requests-2.31.0.tar.gz", hash = "sha256:aaa111", size = 110000 }
wheels = [
    { url = "https://files.pythonhosted.org/requests-2.31.0-py3-none-any.whl", hash = "sha256:bbb222", size = 62000 },
]

[package.optional-dependencies]
security = [
    { name = "pyopenssl" },
    { name = "cryptography" },
]
socks = [
    { name = "pysocks" },
]

[[package]]
name = "click"
version = "8.1.7"
source = { registry = "https://pypi.org/simple" }
wheels = [
    { url = "https://files.pythonhosted.org/click-8.1.7-py3-none-any.whl", hash = "sha256:ccc333", size = 97000 },
]

[[package]]
name = "urllib3"
version = "2.1.0"
source = { registry = "https://pypi.org/simple" }
wheels = [
    { url = "https://files.pythonhosted.org/urllib3-2.1.0-py3-none-any.whl", hash = "sha256:ddd444" },
]

[[package]]
name = "certifi"
version = "2024.2.2"
source = { registry = "https://pypi.org/simple" }
wheels = [
    { hash = "sha256:eee555" },
]

[[package]]
name = "charset-normalizer"
version = "3.3.2"
source = { registry = "https://pypi.org/simple" }
wheels = [
    { hash = "sha256:fff666" },
    { filename = "charset_normalizer-3.3.2-cp312-cp312-manylinux_2_17_x86_64.whl", hash = "sha256:ggg777", size = 140000 },
]

[[package]]
name = "idna"
version = "3.7"
source = { registry = "https://pypi.org/simple" }
wheels = [
    { hash = "sha256:hhh888" },
]

[[package]]
name = "my-lib"
version = "0.5.0"
source = { git = "https://github.com/user/my-lib.git?rev=abc123" }
wheels = [
    { hash = "sha256:iii999" },
]

[[package]]
name = "local-utils"
version = "0.2.0"
source = { path = "./vendored/local-utils-0.2.0.tar.gz" }
wheels = [
    { hash = "sha256:jjj000" },
]

[[package]]
name = "editable-tool"
version = "0.1.0"
source = { editable = "./tools/editable-tool" }

[[package]]
name = "dir-pkg"
version = "0.3.0"
source = { directory = "./libs/dir-pkg" }
wheels = [
    { hash = "sha256:kkk111" },
]
"#;

        let lock = UvLock::from_str(content).unwrap();
        assert_eq!(lock.version, 1);
        assert_eq!(lock.revision, 3);
        assert_eq!(lock.requires_python.as_deref(), Some(">=3.10"));
        assert_eq!(lock.options.resolution_mode.as_deref(), Some("highest"));
        assert_eq!(lock.options.prerelease_mode.as_deref(), Some("disallow"));
        assert_eq!(lock.packages.len(), 11);

        // Verify the virtual root project
        let project = lock.get_package("my-project").unwrap();
        assert_eq!(project.dependencies.len(), 3);
        assert_eq!(project.optional_dependencies.len(), 2);
        assert!(project.optional_dependencies.contains_key("dev"));
        assert!(project.optional_dependencies.contains_key("docs"));
        assert_eq!(project.dev_dependencies.len(), 1);
        assert_eq!(project.dev_dependencies["test"].len(), 2);

        // Verify registry package with optional deps and sdist
        let requests = lock.get_package("requests").unwrap();
        assert_eq!(requests.version.as_deref(), Some("2.31.0"));
        assert_eq!(requests.dependencies.len(), 5);
        assert!(requests.sdist.is_some());
        assert_eq!(requests.sdist.as_ref().unwrap().size, Some(110000));
        assert_eq!(requests.wheels.len(), 1);
        assert_eq!(requests.optional_dependencies.len(), 2);

        // Verify dependency with marker
        let colorama_dep = requests
            .dependencies
            .iter()
            .find(|d| d.name == "colorama")
            .unwrap();
        assert!(colorama_dep.marker.is_some());

        // Verify git source
        let my_lib = lock.get_package("my-lib").unwrap();
        assert!(matches!(&my_lib.source, PackageSource::Git { url } if url.contains("github.com")));

        // Verify path source
        let local_utils = lock.get_package("local-utils").unwrap();
        assert!(matches!(&local_utils.source, PackageSource::Path { .. }));

        // Verify editable source
        let editable = lock.get_package("editable-tool").unwrap();
        assert!(matches!(&editable.source, PackageSource::Editable { .. }));

        // Verify directory source
        let dir_pkg = lock.get_package("dir-pkg").unwrap();
        assert!(matches!(&dir_pkg.source, PackageSource::Directory { .. }));

        // Verify multi-wheel package
        let charset = lock.get_package("charset-normalizer").unwrap();
        assert_eq!(charset.wheels.len(), 2);
        assert!(charset.wheels.iter().any(|w| w.filename.is_some()));
    }

    // ── Edge case: Round-trip with all source types ─────────────

    #[test]
    fn round_trip_all_source_types() {
        let lock = UvLock {
            version: 1,
            revision: 1,
            requires_python: Some(">=3.10".to_string()),
            options: LockOptions::default(),
            packages: vec![
                LockedPackage {
                    name: "registry-pkg".to_string(),
                    version: Some("1.0.0".to_string()),
                    source: PackageSource::Registry {
                        url: "https://pypi.org/simple".to_string(),
                    },
                    dependencies: vec![],
                    optional_dependencies: BTreeMap::new(),
                    dev_dependencies: BTreeMap::new(),
                    sdist: None,
                    wheels: vec![Artifact {
                        url: None,
                        path: None,
                        filename: None,
                        hash: "sha256:reg".to_string(),
                        size: None,
                    }],
                },
                LockedPackage {
                    name: "git-pkg".to_string(),
                    version: Some("2.0.0".to_string()),
                    source: PackageSource::Git {
                        url: "https://github.com/user/repo.git?rev=main".to_string(),
                    },
                    dependencies: vec![],
                    optional_dependencies: BTreeMap::new(),
                    dev_dependencies: BTreeMap::new(),
                    sdist: None,
                    wheels: vec![Artifact {
                        url: None,
                        path: None,
                        filename: None,
                        hash: "sha256:git".to_string(),
                        size: None,
                    }],
                },
                LockedPackage {
                    name: "path-pkg".to_string(),
                    version: Some("3.0.0".to_string()),
                    source: PackageSource::Path {
                        path: "./vendored/path-pkg.tar.gz".to_string(),
                    },
                    dependencies: vec![],
                    optional_dependencies: BTreeMap::new(),
                    dev_dependencies: BTreeMap::new(),
                    sdist: None,
                    wheels: vec![Artifact {
                        url: None,
                        path: None,
                        filename: None,
                        hash: "sha256:pth".to_string(),
                        size: None,
                    }],
                },
                LockedPackage {
                    name: "dir-pkg".to_string(),
                    version: Some("4.0.0".to_string()),
                    source: PackageSource::Directory {
                        path: "./libs/dir-pkg".to_string(),
                    },
                    dependencies: vec![],
                    optional_dependencies: BTreeMap::new(),
                    dev_dependencies: BTreeMap::new(),
                    sdist: None,
                    wheels: vec![Artifact {
                        url: None,
                        path: None,
                        filename: None,
                        hash: "sha256:dir".to_string(),
                        size: None,
                    }],
                },
                LockedPackage {
                    name: "editable-pkg".to_string(),
                    version: Some("5.0.0".to_string()),
                    source: PackageSource::Editable {
                        path: "./packages/editable-pkg".to_string(),
                    },
                    dependencies: vec![],
                    optional_dependencies: BTreeMap::new(),
                    dev_dependencies: BTreeMap::new(),
                    sdist: None,
                    wheels: vec![Artifact {
                        url: None,
                        path: None,
                        filename: None,
                        hash: "sha256:edi".to_string(),
                        size: None,
                    }],
                },
                LockedPackage {
                    name: "virtual-pkg".to_string(),
                    version: Some("6.0.0".to_string()),
                    source: PackageSource::Virtual {
                        path: ".".to_string(),
                    },
                    dependencies: vec![],
                    optional_dependencies: BTreeMap::new(),
                    dev_dependencies: BTreeMap::new(),
                    sdist: None,
                    wheels: vec![],
                },
            ],
        };

        let toml_str = lock.to_toml().unwrap();
        let reparsed = UvLock::from_str(&toml_str).unwrap();

        assert_eq!(lock.packages.len(), reparsed.packages.len());
        for orig_pkg in &lock.packages {
            let re_pkg = reparsed.get_package(&orig_pkg.name).unwrap();
            assert_eq!(
                orig_pkg.source, re_pkg.source,
                "source mismatch for {}",
                orig_pkg.name
            );
            assert_eq!(
                orig_pkg.version, re_pkg.version,
                "version mismatch for {}",
                orig_pkg.name
            );
        }
    }

    // ── Edge case: Lockfile with environment markers on dependencies ─

    #[test]
    fn round_trip_dependencies_with_markers() {
        let lock = UvLock {
            version: 1,
            revision: 1,
            requires_python: None,
            options: LockOptions::default(),
            packages: vec![LockedPackage {
                name: "parent".to_string(),
                version: Some("1.0.0".to_string()),
                source: PackageSource::Registry {
                    url: "https://pypi.org/simple".to_string(),
                },
                dependencies: vec![
                    Dependency {
                        name: "colorama".to_string(),
                        version: Some("0.4.6".to_string()),
                        source: None,
                        marker: Some("sys_platform == \"win32\"".to_string()),
                        extra: None,
                    },
                    Dependency {
                        name: "readline".to_string(),
                        version: Some("1.0.0".to_string()),
                        source: None,
                        marker: Some("os_name == \"posix\"".to_string()),
                        extra: None,
                    },
                ],
                optional_dependencies: BTreeMap::new(),
                dev_dependencies: BTreeMap::new(),
                sdist: None,
                wheels: vec![Artifact {
                    url: None,
                    path: None,
                    filename: None,
                    hash: "sha256:abc".to_string(),
                    size: None,
                }],
            }],
        };

        let toml_str = lock.to_toml().unwrap();
        let reparsed = UvLock::from_str(&toml_str).unwrap();
        let pkg = reparsed.get_package("parent").unwrap();
        assert_eq!(pkg.dependencies.len(), 2);

        let colorama = pkg
            .dependencies
            .iter()
            .find(|d| d.name == "colorama")
            .unwrap();
        assert_eq!(
            colorama.marker.as_deref(),
            Some("sys_platform == \"win32\"")
        );
        assert_eq!(colorama.version.as_deref(), Some("0.4.6"));

        let readline = pkg
            .dependencies
            .iter()
            .find(|d| d.name == "readline")
            .unwrap();
        assert_eq!(readline.marker.as_deref(), Some("os_name == \"posix\""));
    }

    // ── Edge case: Empty lockfile with header but zero packages ──

    #[test]
    fn empty_lockfile_with_requires_python() {
        let content = r#"
version = 1
revision = 5
requires-python = ">=3.11"

[options]
resolution-mode = "lowest"
"#;

        let lock = UvLock::from_str(content).unwrap();
        assert_eq!(lock.version, 1);
        assert_eq!(lock.revision, 5);
        assert_eq!(lock.requires_python.as_deref(), Some(">=3.11"));
        assert_eq!(lock.options.resolution_mode.as_deref(), Some("lowest"));
        assert!(lock.packages.is_empty());

        // Round-trip: writing and re-parsing should yield the same
        let toml_str = lock.to_toml().unwrap();
        let reparsed = UvLock::from_str(&toml_str).unwrap();
        assert!(reparsed.packages.is_empty());
        assert_eq!(reparsed.requires_python.as_deref(), Some(">=3.11"));
        assert_eq!(reparsed.options.resolution_mode.as_deref(), Some("lowest"));
    }

    #[test]
    fn round_trip_package_name_with_newline() {
        let lock = UvLock {
            version: 1,
            revision: 1,
            requires_python: None,
            options: LockOptions::default(),
            packages: vec![LockedPackage {
                name: "foo\nbar".to_string(),
                version: Some("1.0.0".to_string()),
                source: PackageSource::Registry {
                    url: "https://pypi.org/simple".to_string(),
                },
                dependencies: vec![],
                optional_dependencies: BTreeMap::new(),
                dev_dependencies: BTreeMap::new(),
                sdist: None,
                wheels: vec![Artifact {
                    url: None,
                    path: None,
                    filename: None,
                    hash: "sha256:eee".to_string(),
                    size: None,
                }],
            }],
        };

        let toml_str = lock.to_toml().unwrap();
        let reparsed = UvLock::from_str(&toml_str).unwrap();
        assert_eq!(reparsed.packages[0].name, "foo\nbar");
    }

    // ── Test: Round-trip with all field types ────────────────────

    #[test]
    fn full_round_trip() {
        let original = UvLock {
            version: 1,
            revision: 3,
            requires_python: Some(">=3.12".to_string()),
            options: LockOptions {
                resolution_mode: Some("lowest".to_string()),
                prerelease_mode: None,
                resolution_markers: None,
            },
            packages: vec![
                LockedPackage {
                    name: "my-app".to_string(),
                    version: Some("0.1.0".to_string()),
                    source: PackageSource::Virtual {
                        path: ".".to_string(),
                    },
                    dependencies: vec![Dependency {
                        name: "requests".to_string(),
                        version: None,
                        source: None,
                        marker: None,
                        extra: Some(vec!["security".to_string()]),
                    }],
                    optional_dependencies: BTreeMap::new(),
                    dev_dependencies: {
                        let mut m = BTreeMap::new();
                        m.insert(
                            "dev".to_string(),
                            vec![Dependency {
                                name: "pytest".to_string(),
                                version: None,
                                source: None,
                                marker: None,
                                extra: None,
                            }],
                        );
                        m
                    },
                    sdist: None,
                    wheels: vec![],
                },
                LockedPackage {
                    name: "requests".to_string(),
                    version: Some("2.32.4".to_string()),
                    source: PackageSource::Registry {
                        url: "https://pypi.org/simple".to_string(),
                    },
                    dependencies: vec![
                        Dependency {
                            name: "certifi".to_string(),
                            version: None,
                            source: None,
                            marker: None,
                            extra: None,
                        },
                        Dependency {
                            name: "urllib3".to_string(),
                            version: None,
                            source: None,
                            marker: None,
                            extra: None,
                        },
                    ],
                    optional_dependencies: {
                        let mut m = BTreeMap::new();
                        m.insert(
                            "security".to_string(),
                            vec![Dependency {
                                name: "pyopenssl".to_string(),
                                version: None,
                                source: None,
                                marker: None,
                                extra: None,
                            }],
                        );
                        m
                    },
                    dev_dependencies: BTreeMap::new(),
                    sdist: Some(Artifact {
                        url: Some("https://example.com/requests-2.32.4.tar.gz".to_string()),
                        path: None,
                        filename: None,
                        hash: "sha256:abc123".to_string(),
                        size: Some(131234),
                    }),
                    wheels: vec![Artifact {
                        url: Some(
                            "https://example.com/requests-2.32.4-py3-none-any.whl".to_string(),
                        ),
                        path: None,
                        filename: None,
                        hash: "sha256:def456".to_string(),
                        size: Some(63456),
                    }],
                },
            ],
        };

        let toml_str = original.to_toml().unwrap();
        let reparsed = UvLock::from_str(&toml_str).unwrap();

        assert_eq!(original.version, reparsed.version);
        assert_eq!(original.revision, reparsed.revision);
        assert_eq!(original.requires_python, reparsed.requires_python);
        assert_eq!(original.options, reparsed.options);
        assert_eq!(original.packages.len(), reparsed.packages.len());

        for orig_pkg in &original.packages {
            let re_pkg = reparsed.get_package(&orig_pkg.name).unwrap();
            assert_eq!(orig_pkg, re_pkg);
        }
    }

    // ── Test: Resolution markers round-trip ────────────────────────

    #[test]
    fn resolution_markers_round_trip() {
        let markers = vec![
            "sys_platform == \"linux\" and platform_machine == \"x86_64\"".to_string(),
            "sys_platform == \"darwin\"".to_string(),
            "sys_platform == \"win32\"".to_string(),
        ];

        let lock = UvLock {
            version: 1,
            revision: 3,
            requires_python: Some(">=3.12".to_string()),
            options: LockOptions {
                resolution_mode: None,
                prerelease_mode: None,
                resolution_markers: Some(markers.clone()),
            },
            packages: vec![],
        };

        let toml_str = lock.to_toml().unwrap();
        assert!(
            toml_str.contains("resolution-markers"),
            "output should contain resolution-markers"
        );

        let reparsed = UvLock::from_str(&toml_str).unwrap();
        assert_eq!(
            reparsed.options.resolution_markers,
            Some(markers),
            "resolution-markers should survive round-trip"
        );
        assert!(reparsed.is_universal());
    }

    // ── Test: Universal lockfile package filtering ──────────────────

    #[test]
    fn packages_for_environment_filtering() {
        // Create a lockfile with packages that have marker-annotated deps.
        let lock = UvLock {
            version: 1,
            revision: 3,
            requires_python: Some(">=3.12".to_string()),
            options: LockOptions {
                resolution_mode: None,
                prerelease_mode: None,
                resolution_markers: Some(vec![
                    "sys_platform == \"linux\"".to_string(),
                    "sys_platform == \"win32\"".to_string(),
                ]),
            },
            packages: vec![
                LockedPackage {
                    name: "myapp".to_string(),
                    version: Some("1.0.0".to_string()),
                    source: PackageSource::Virtual {
                        path: ".".to_string(),
                    },
                    dependencies: vec![
                        Dependency {
                            name: "common".to_string(),
                            version: None,
                            source: None,
                            marker: None, // no marker = all platforms
                            extra: None,
                        },
                        Dependency {
                            name: "linux-only".to_string(),
                            version: None,
                            source: None,
                            marker: Some("sys_platform == \"linux\"".to_string()),
                            extra: None,
                        },
                        Dependency {
                            name: "win-only".to_string(),
                            version: None,
                            source: None,
                            marker: Some("sys_platform == \"win32\"".to_string()),
                            extra: None,
                        },
                    ],
                    optional_dependencies: BTreeMap::new(),
                    dev_dependencies: BTreeMap::new(),
                    sdist: None,
                    wheels: vec![],
                },
                LockedPackage {
                    name: "common".to_string(),
                    version: Some("1.0.0".to_string()),
                    source: PackageSource::default(),
                    dependencies: vec![],
                    optional_dependencies: BTreeMap::new(),
                    dev_dependencies: BTreeMap::new(),
                    sdist: None,
                    wheels: vec![],
                },
                LockedPackage {
                    name: "linux-only".to_string(),
                    version: Some("1.0.0".to_string()),
                    source: PackageSource::default(),
                    dependencies: vec![],
                    optional_dependencies: BTreeMap::new(),
                    dev_dependencies: BTreeMap::new(),
                    sdist: None,
                    wheels: vec![],
                },
                LockedPackage {
                    name: "win-only".to_string(),
                    version: Some("1.0.0".to_string()),
                    source: PackageSource::default(),
                    dependencies: vec![],
                    optional_dependencies: BTreeMap::new(),
                    dev_dependencies: BTreeMap::new(),
                    sdist: None,
                    wheels: vec![],
                },
            ],
        };

        // Simulate a Linux environment: sys_platform == "linux" is true
        let linux_pkgs = lock.packages_for_environment(&|marker| marker.contains("linux"));
        let linux_set: std::collections::HashSet<String> = linux_pkgs
            .into_iter()
            .map(|n| normalize_pep503(&n))
            .collect();

        assert!(linux_set.contains("myapp"), "myapp should be included");
        assert!(linux_set.contains("common"), "common should be included");
        assert!(
            linux_set.contains("linux-only"),
            "linux-only should be included on linux"
        );
        assert!(
            !linux_set.contains("win-only"),
            "win-only should NOT be included on linux"
        );

        // Simulate a Windows environment: sys_platform == "win32" is true
        let win_pkgs = lock.packages_for_environment(&|marker| marker.contains("win32"));
        let win_set: std::collections::HashSet<String> =
            win_pkgs.into_iter().map(|n| normalize_pep503(&n)).collect();

        assert!(win_set.contains("myapp"), "myapp should be included");
        assert!(win_set.contains("common"), "common should be included");
        assert!(
            !win_set.contains("linux-only"),
            "linux-only should NOT be included on windows"
        );
        assert!(
            win_set.contains("win-only"),
            "win-only should be included on windows"
        );
    }

    #[test]
    fn test_bfs_root_seeding_excludes_referenced_packages() {
        // Create a lockfile where:
        // - Package "root-app" depends on "lib-a" unconditionally
        // - Package "lib-a" depends on "lib-b" unconditionally
        // Only "root-app" should be a BFS root, but all 3 should be reachable.
        let lock = UvLock {
            version: 1,
            revision: 3,
            requires_python: Some(">=3.12".to_string()),
            options: LockOptions {
                resolution_mode: None,
                prerelease_mode: None,
                resolution_markers: Some(vec![
                    "sys_platform == \"linux\"".to_string(),
                    "sys_platform == \"win32\"".to_string(),
                ]),
            },
            packages: vec![
                LockedPackage {
                    name: "root-app".to_string(),
                    version: Some("1.0.0".to_string()),
                    source: PackageSource::Virtual {
                        path: ".".to_string(),
                    },
                    dependencies: vec![Dependency {
                        name: "lib-a".to_string(),
                        version: None,
                        source: None,
                        marker: None,
                        extra: None,
                    }],
                    optional_dependencies: BTreeMap::new(),
                    dev_dependencies: BTreeMap::new(),
                    sdist: None,
                    wheels: vec![],
                },
                LockedPackage {
                    name: "lib-a".to_string(),
                    version: Some("1.0.0".to_string()),
                    source: PackageSource::default(),
                    dependencies: vec![Dependency {
                        name: "lib-b".to_string(),
                        version: None,
                        source: None,
                        marker: None,
                        extra: None,
                    }],
                    optional_dependencies: BTreeMap::new(),
                    dev_dependencies: BTreeMap::new(),
                    sdist: None,
                    wheels: vec![],
                },
                LockedPackage {
                    name: "lib-b".to_string(),
                    version: Some("1.0.0".to_string()),
                    source: PackageSource::default(),
                    dependencies: vec![],
                    optional_dependencies: BTreeMap::new(),
                    dev_dependencies: BTreeMap::new(),
                    sdist: None,
                    wheels: vec![],
                },
            ],
        };

        let pkgs = lock.packages_for_environment(&|_marker| true);
        let pkg_set: std::collections::HashSet<String> =
            pkgs.into_iter().map(|n| normalize_pep503(&n)).collect();

        // All 3 packages should be reachable via BFS from the true root
        assert!(pkg_set.contains("root-app"), "root-app should be included");
        assert!(pkg_set.contains("lib-a"), "lib-a should be included");
        assert!(pkg_set.contains("lib-b"), "lib-b should be included");
        assert_eq!(
            pkg_set.len(),
            3,
            "exactly 3 packages should be in the result"
        );
    }
}
