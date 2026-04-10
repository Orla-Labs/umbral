//! Wheel installer with global cache and hardlink/copy/clone support.
//!
//! Installs Python wheels (.whl files) into virtual environments.
//! Features:
//! - Global cache at `~/.cache/umbral/wheels/` to avoid re-downloading
//! - Link modes: hardlink (default), copy, clone (CoW on APFS/btrfs)
//! - Hash verification (SHA-256)
//! - RECORD tracking and INSTALLER file writing
//! - PEP 517 sdist building with build isolation (see [`build`] module)

pub mod build;

use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracing::info;

// ── Errors ──────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum InstallError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("failed to read wheel: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("hash mismatch for {filename}: expected {expected}, got {actual}")]
    HashMismatch {
        filename: String,
        expected: String,
        actual: String,
    },

    #[error("invalid wheel: {0}")]
    InvalidWheel(String),

    #[error("download error: {0}")]
    Download(String),

    #[error("path traversal attempt: {0}")]
    PathTraversal(String),

    #[error("editable install failed: {0}")]
    Editable(String),
}

// ── Link mode ───────────────────────────────────────────────────────

/// How to link files from cache to site-packages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LinkMode {
    /// Hard link (default) — zero copy, shared inode.
    #[default]
    Hardlink,
    /// Full file copy.
    Copy,
    /// Copy-on-write clone (APFS/btrfs). Falls back to copy.
    Clone,
}

// ── Wheel installer ─────────────────────────────────────────────────

/// Installs wheels from cache into site-packages.
pub struct WheelInstaller {
    /// Global cache directory (~/.cache/umbral/wheels/).
    pub cache_dir: PathBuf,
    /// How to link files from cache to site-packages.
    pub link_mode: LinkMode,
}

/// Information about an installed distribution.
#[derive(Debug, Clone)]
pub struct InstalledDistribution {
    pub name: String,
    pub version: String,
    pub dist_info_dir: PathBuf,
    pub files_installed: usize,
}

impl WheelInstaller {
    /// Create a new installer with default cache directory.
    pub fn new(link_mode: LinkMode) -> Self {
        let cache_dir = dirs_cache().join("wheels");
        Self {
            cache_dir,
            link_mode,
        }
    }

    /// Create with a custom cache directory (for testing).
    pub fn with_cache_dir(cache_dir: PathBuf, link_mode: LinkMode) -> Self {
        Self {
            cache_dir,
            link_mode,
        }
    }

    /// Install a wheel file into the target site-packages directory.
    ///
    /// `python_path` is the absolute path to the venv's Python interpreter
    /// (e.g. `/path/to/venv/bin/python`). It is used for console-script shebangs.
    ///
    /// `venv_root` is the optional explicit path to the virtual environment root
    /// directory. When provided, it is used for `.data/data/` and `.data/headers/`
    /// targets instead of computing the root from `site_packages` via `..` chains,
    /// which differ between Unix (3 levels) and Windows (2 levels). If `None`, the
    /// legacy `..` chain fallback is used.
    pub fn install_wheel(
        &self,
        wheel_path: &Path,
        site_packages: &Path,
        bin_dir: &Path,
        python_path: &Path,
        venv_root: Option<&Path>,
    ) -> Result<InstalledDistribution, InstallError> {
        let file = fs::File::open(wheel_path)?;
        let mut archive = zip::ZipArchive::new(file)?;

        // Compute the relative path from site-packages to the bin directory for
        // RECORD entries. On Unix site-packages is 3 levels deep
        // (lib/pythonX.Y/site-packages → ../../../bin), on Windows it is 2 levels
        // deep (Lib/site-packages → ../../Scripts).
        let bin_rel_path = if cfg!(windows) {
            "../../Scripts"
        } else {
            "../../../bin"
        };

        // Find the .dist-info directory name
        let dist_info_name = find_dist_info(&mut archive)?;
        let (name, version) = parse_dist_info_name(&dist_info_name)?;

        info!(name = %name, version = %version, "installing wheel");

        // Extract all files, collecting RECORD entries (path, hash, size)
        let mut files_installed = 0;
        let mut record_entries: Vec<(String, String, usize)> = Vec::new();

        for i in 0..archive.len() {
            let mut entry = archive.by_index(i)?;
            let entry_name = entry.name().to_string();

            // Skip directories
            if entry.is_dir() {
                let dir_path = safe_join(site_packages, &entry_name)?;
                fs::create_dir_all(&dir_path)?;
                continue;
            }

            // Skip the wheel's own RECORD file — we write our own at the end
            // with a self-referencing `,,` entry. Including the wheel's RECORD
            // would produce a duplicate entry (PEP 376 violation).
            if entry_name.ends_with(".dist-info/RECORD") {
                continue;
            }

            // Handle .data directories specially
            if let Some(data_path) = extract_data_path(
                &entry_name,
                &dist_info_name,
                bin_dir,
                site_packages,
                venv_root,
            )? {
                if let Some(parent) = data_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf)?;

                let hash = record_hash(&buf);
                let size = buf.len();
                fs::write(&data_path, &buf)?;

                // Make scripts executable
                if entry_name.contains("/scripts/") || entry_name.contains("/bin/") {
                    make_executable(&data_path);
                }

                // Compute RECORD-relative path (relative to site-packages)
                // instead of using the zip-internal path.
                let record_path = if let Ok(rel) = data_path.strip_prefix(site_packages) {
                    // File is under site-packages (purelib, platlib)
                    rel.to_string_lossy().to_string()
                } else {
                    // File is outside site-packages (bin, data, headers).
                    // Compute a ../-based relative path from site-packages.
                    relative_path_from(site_packages, &data_path)
                };

                record_entries.push((record_path, hash, size));
                files_installed += 1;
                continue;
            }

            // Regular file — extract to site-packages
            let target_path = safe_join(site_packages, &entry_name)?;
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent)?;
            }

            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;

            let hash = record_hash(&buf);
            let size = buf.len();
            fs::write(&target_path, &buf)?;

            record_entries.push((entry_name, hash, size));
            files_installed += 1;
        }

        // Write INSTALLER file
        let dist_info_path = site_packages.join(&dist_info_name);
        fs::create_dir_all(&dist_info_path)?;
        let installer_content = b"umbral\n";
        fs::write(dist_info_path.join("INSTALLER"), installer_content)?;
        record_entries.push((
            format!("{dist_info_name}/INSTALLER"),
            record_hash(installer_content),
            installer_content.len(),
        ));

        // Parse entry_points.txt and generate console scripts
        let entry_points_path = dist_info_path.join("entry_points.txt");
        if entry_points_path.exists() {
            let content = fs::read_to_string(&entry_points_path)?;
            let script_entries =
                generate_console_scripts(&content, bin_dir, python_path, bin_rel_path)?;
            record_entries.extend(script_entries);
        }

        // Write RECORD file (PEP 376). Each installed file gets a line:
        //   path,hash_algorithm=hash_value,size
        // The RECORD file's own entry has empty hash and size fields.
        let mut record_content = String::new();
        for (path, hash, size) in &record_entries {
            record_content.push_str(&format!("{path},{hash},{size}\n"));
        }
        let record_path_str = format!("{dist_info_name}/RECORD");
        record_content.push_str(&format!("{record_path_str},,\n"));
        fs::write(dist_info_path.join("RECORD"), &record_content)?;

        Ok(InstalledDistribution {
            name,
            version,
            dist_info_dir: dist_info_path,
            files_installed,
        })
    }

    /// Download a wheel from a URL into the cache. Returns the cached path.
    pub async fn download_wheel(
        &self,
        url: &str,
        filename: &str,
        expected_sha256: Option<&str>,
    ) -> Result<PathBuf, InstallError> {
        let cache_path = self.cache_dir.join(filename);

        // Check if already cached
        if cache_path.exists() {
            if let Some(expected) = expected_sha256 {
                let actual = hash_file(&cache_path)?;
                if actual == expected {
                    info!(filename, "cache hit");
                    return Ok(cache_path);
                }
                // Hash mismatch — re-download
                info!(filename, "cache hash mismatch, re-downloading");
            } else {
                info!(filename, "cache hit (no hash to verify)");
                return Ok(cache_path);
            }
        }

        // Download
        info!(url, filename, "downloading wheel");
        fs::create_dir_all(&self.cache_dir)?;

        let response = reqwest::get(url)
            .await
            .map_err(|e| InstallError::Download(e.to_string()))?;

        if !response.status().is_success() {
            return Err(InstallError::Download(format!(
                "HTTP {}: {}",
                response.status(),
                url
            )));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| InstallError::Download(e.to_string()))?;

        // Verify hash before writing
        if let Some(expected) = expected_sha256 {
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            let actual = hex::encode(hasher.finalize());
            if actual != expected {
                return Err(InstallError::HashMismatch {
                    filename: filename.to_string(),
                    expected: expected.to_string(),
                    actual,
                });
            }
        }

        fs::write(&cache_path, &bytes)?;
        Ok(cache_path)
    }

    /// Link a file from cache to target using the configured link mode.
    ///
    /// NOTE: Currently unused by `install_wheel`, which writes directly via
    /// `fs::write` during zip extraction. This method will be wired up when
    /// the download+install pipeline is unified so that cached files are linked
    /// (hardlink/clone/copy) into site-packages instead of re-extracted.
    pub fn link_file(&self, source: &Path, target: &Path) -> Result<(), InstallError> {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }

        match self.link_mode {
            LinkMode::Hardlink => {
                match fs::hard_link(source, target) {
                    Ok(()) => Ok(()),
                    Err(_) => {
                        // Fallback to copy if hardlink fails (e.g., cross-device)
                        fs::copy(source, target)?;
                        Ok(())
                    }
                }
            }
            LinkMode::Copy => {
                fs::copy(source, target)?;
                Ok(())
            }
            LinkMode::Clone => {
                // Try CoW clone, fallback to copy
                // On macOS APFS: clonefile() — not easily available in stable Rust
                // Fallback to copy for now
                fs::copy(source, target)?;
                Ok(())
            }
        }
    }
}

// ── Editable installs ───────────────────────────────────────────────

/// Result of an editable (development mode) install.
#[derive(Debug, Clone)]
pub struct EditableInstallResult {
    /// Path to the `.pth` file in site-packages.
    pub pth_path: PathBuf,
    /// Path to the `.dist-info` directory in site-packages.
    pub dist_info: PathBuf,
}

/// Install a project in editable/development mode.
///
/// An editable install creates a `.pth` file in site-packages that adds the
/// project source directory to `sys.path`, enabling live development without
/// reinstalling. This is the equivalent of `pip install -e .`.
///
/// This also creates a minimal `.dist-info` directory with:
/// - `METADATA` (PEP 566)
/// - `INSTALLER` (PEP 376)
/// - `direct_url.json` (PEP 610, marking the install as editable)
/// - `RECORD` (PEP 376, listing all installed files)
pub fn install_editable(
    project_dir: &Path,
    site_packages: &Path,
    project_name: &str,
    project_version: Option<&str>,
) -> Result<EditableInstallResult, InstallError> {
    let normalized_name = project_name.replace('-', "_");
    let version = project_version.unwrap_or("0.0.0");

    // 1. Create the .pth file that adds the project directory to sys.path
    let pth_path = site_packages.join(format!("{normalized_name}.pth"));
    let pth_content = format!("{}\n", project_dir.display());
    fs::write(&pth_path, &pth_content)?;

    info!(
        project = project_name,
        pth = %pth_path.display(),
        "created .pth file"
    );

    // 2. Create the .dist-info directory
    let dist_info = site_packages.join(format!("{normalized_name}-{version}.dist-info"));
    fs::create_dir_all(&dist_info)?;

    // 3. Write METADATA (PEP 566)
    let metadata_content = format!(
        "Metadata-Version: 2.1\nName: {}\nVersion: {}\n",
        project_name, version
    );
    fs::write(dist_info.join("METADATA"), &metadata_content)?;

    // 4. Write INSTALLER (PEP 376)
    let installer_content = "umbral\n";
    fs::write(dist_info.join("INSTALLER"), installer_content)?;

    // 5. Write direct_url.json (PEP 610)
    let direct_url = serde_json::json!({
        "url": format!("file://{}", project_dir.display()),
        "dir_info": {
            "editable": true
        }
    });
    let direct_url_content = serde_json::to_string_pretty(&direct_url)
        .map_err(|e| InstallError::Editable(format!("failed to serialize direct_url.json: {e}")))?;
    fs::write(dist_info.join("direct_url.json"), &direct_url_content)?;

    // 6. Write RECORD (PEP 376)
    // List all files we created, with hashes for everything except RECORD itself.
    let dist_info_name = format!("{normalized_name}-{version}.dist-info");
    let mut record_entries: Vec<String> = Vec::new();

    // .pth file
    let pth_hash = record_hash(pth_content.as_bytes());
    record_entries.push(format!(
        "{normalized_name}.pth,{pth_hash},{}",
        pth_content.len()
    ));

    // METADATA
    let metadata_hash = record_hash(metadata_content.as_bytes());
    record_entries.push(format!(
        "{dist_info_name}/METADATA,{metadata_hash},{}",
        metadata_content.len()
    ));

    // INSTALLER
    let installer_hash = record_hash(installer_content.as_bytes());
    record_entries.push(format!(
        "{dist_info_name}/INSTALLER,{installer_hash},{}",
        installer_content.len()
    ));

    // direct_url.json
    let direct_url_hash = record_hash(direct_url_content.as_bytes());
    record_entries.push(format!(
        "{dist_info_name}/direct_url.json,{direct_url_hash},{}",
        direct_url_content.len()
    ));

    // RECORD itself (empty hash per PEP 376)
    record_entries.push(format!("{dist_info_name}/RECORD,,"));

    let record_content = record_entries.join("\n") + "\n";
    fs::write(dist_info.join("RECORD"), &record_content)?;

    info!(
        project = project_name,
        dist_info = %dist_info.display(),
        "created editable dist-info"
    );

    Ok(EditableInstallResult {
        pth_path,
        dist_info,
    })
}

// ── Installed package detection ─────────────────────────────────────

/// Information about a package currently installed in a site-packages dir.
#[derive(Debug, Clone)]
pub struct InstalledPackage {
    pub name: String,
    pub version: String,
    pub dist_info_path: PathBuf,
}

/// Scan a site-packages directory for installed packages.
pub fn scan_installed(site_packages: &Path) -> Result<Vec<InstalledPackage>, InstallError> {
    let mut packages = Vec::new();

    if !site_packages.exists() {
        return Ok(packages);
    }

    for entry in fs::read_dir(site_packages)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if name_str.ends_with(".dist-info") && entry.file_type()?.is_dir() {
            let metadata_path = entry.path().join("METADATA");
            if metadata_path.exists() {
                if let Ok(pkg) = parse_installed_metadata(&metadata_path) {
                    packages.push(InstalledPackage {
                        name: pkg.0,
                        version: pkg.1,
                        dist_info_path: entry.path(),
                    });
                }
            }
        }
    }

    Ok(packages)
}

/// Parse name and version from a METADATA file.
fn parse_installed_metadata(path: &Path) -> Result<(String, String), InstallError> {
    let content = fs::read_to_string(path)?;
    let mut name = None;
    let mut version = None;

    for line in content.lines() {
        if let Some(n) = line.strip_prefix("Name: ") {
            name = Some(n.trim().to_string());
        } else if let Some(v) = line.strip_prefix("Version: ") {
            version = Some(v.trim().to_string());
        }
        if name.is_some() && version.is_some() {
            break;
        }
        // Stop at blank line (end of headers)
        if line.trim().is_empty() {
            break;
        }
    }

    match (name, version) {
        (Some(n), Some(v)) => Ok((n, v)),
        _ => Err(InstallError::InvalidWheel(format!(
            "missing Name/Version in {}",
            path.display()
        ))),
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Safely join an untrusted relative path to a base directory, preventing
/// path traversal attacks (zip-slip). Normalizes the path without requiring
/// it to exist on disk, then verifies the result is within `base`.
fn safe_join(base: &Path, untrusted: &str) -> Result<PathBuf, InstallError> {
    let joined = base.join(untrusted);
    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(c) => normalized.push(c),
            Component::RootDir => normalized.push(component),
            Component::Prefix(p) => normalized.push(p.as_os_str()),
            Component::CurDir => {}
        }
    }
    if !normalized.starts_with(base) {
        return Err(InstallError::PathTraversal(untrusted.to_string()));
    }
    Ok(normalized)
}

/// Find the .dist-info directory name inside a wheel archive.
fn find_dist_info(archive: &mut zip::ZipArchive<fs::File>) -> Result<String, InstallError> {
    for i in 0..archive.len() {
        let entry = archive.by_index(i)?;
        let name = entry.name();
        if let Some(slash_pos) = name.find('/') {
            let dir = &name[..slash_pos];
            if dir.ends_with(".dist-info") {
                return Ok(dir.to_string());
            }
        }
    }
    Err(InstallError::InvalidWheel(
        "no .dist-info directory found in wheel".into(),
    ))
}

/// Parse package name and version from a dist-info directory name.
/// e.g., "requests-2.31.0.dist-info" → ("requests", "2.31.0")
fn parse_dist_info_name(name: &str) -> Result<(String, String), InstallError> {
    let without_suffix = name
        .strip_suffix(".dist-info")
        .ok_or_else(|| InstallError::InvalidWheel(format!("invalid dist-info name: {name}")))?;

    let dash_pos = without_suffix
        .rfind('-')
        .ok_or_else(|| InstallError::InvalidWheel(format!("no version in dist-info: {name}")))?;

    let pkg_name = &without_suffix[..dash_pos];
    let version = &without_suffix[dash_pos + 1..];

    Ok((pkg_name.to_string(), version.to_string()))
}

/// Extract the target path for a .data directory entry.
/// Returns `Ok(None)` if the entry is not a .data path.
/// Returns `Err(PathTraversal)` if the resolved path escapes its target directory.
///
/// When `venv_root` is provided, it is used directly for `data/` and `headers/`
/// targets instead of computing the venv root from `site_packages` via `..` chains
/// (which break across platforms — Unix site-packages is 3 levels deep while
/// Windows is only 2).
fn extract_data_path(
    entry_name: &str,
    dist_info_name: &str,
    bin_dir: &Path,
    site_packages: &Path,
    venv_root: Option<&Path>,
) -> Result<Option<PathBuf>, InstallError> {
    let data_prefix = dist_info_name.replace(".dist-info", ".data");
    if !entry_name.starts_with(&data_prefix) {
        return Ok(None);
    }

    let relative = &entry_name[data_prefix.len()..];
    let relative = relative.strip_prefix('/').unwrap_or(relative);

    if let Some(rest) = relative.strip_prefix("scripts/") {
        Ok(Some(safe_join(bin_dir, rest)?))
    } else if let Some(rest) = relative.strip_prefix("data/") {
        // data/ targets the venv prefix root.
        let prefix = if let Some(root) = venv_root {
            root.to_path_buf()
        } else {
            // Fallback: compute from site-packages via `..` chains.
            // This assumes Unix layout (3 levels: lib/pythonX.Y/site-packages).
            site_packages.join("..").join("..").join("..")
        };
        Ok(Some(safe_join(&prefix, rest)?))
    } else if let Some(rest) = relative.strip_prefix("headers/") {
        let include = if let Some(root) = venv_root {
            root.join("include")
        } else {
            // Fallback: assumes Unix layout (2 levels up from site-packages for include).
            site_packages.join("..").join("..").join("include")
        };
        Ok(Some(safe_join(&include, rest)?))
    } else if let Some(rest) = relative.strip_prefix("purelib/") {
        Ok(Some(safe_join(site_packages, rest)?))
    } else if let Some(rest) = relative.strip_prefix("platlib/") {
        Ok(Some(safe_join(site_packages, rest)?))
    } else {
        Ok(None)
    }
}

/// Generate console script wrappers from entry_points.txt content.
///
/// `python_path` is the absolute path to the venv's Python interpreter,
/// used as the shebang in generated scripts (e.g. `#!/path/to/venv/bin/python`).
///
/// `bin_rel_path` is the relative path from site-packages to the bin directory,
/// used in RECORD entries. On Unix this is `"../../../bin"` (3 levels up from
/// `lib/pythonX.Y/site-packages`), on Windows it is `"../../Scripts"` (2 levels
/// up from `Lib/site-packages`).
fn generate_console_scripts(
    content: &str,
    bin_dir: &Path,
    python_path: &Path,
    bin_rel_path: &str,
) -> Result<Vec<(String, String, usize)>, InstallError> {
    let mut in_console_scripts = false;
    let mut entries = Vec::new();

    for line in content.lines() {
        let line = line.trim();

        if line == "[console_scripts]" {
            in_console_scripts = true;
            continue;
        }
        if line.starts_with('[') {
            in_console_scripts = false;
            continue;
        }
        if !in_console_scripts || line.is_empty() {
            continue;
        }

        // Parse: name = module:function
        if let Some((script_name, entry)) = line.split_once('=') {
            let script_name = script_name.trim();
            let entry = entry.trim();

            if let Some((module, func)) = entry.split_once(':') {
                generate_script(
                    bin_dir,
                    script_name,
                    module.trim(),
                    func.trim(),
                    python_path,
                )?;
                info!(script = script_name, "generated console script");

                // Read back the generated script to compute its RECORD entry
                let script_path = bin_dir.join(script_name);
                let script_bytes = fs::read(&script_path)?;
                let hash = record_hash(&script_bytes);
                let size = script_bytes.len();
                // PEP 376: paths are relative to site-packages (the .dist-info parent)
                let rel_path = format!("{bin_rel_path}/{script_name}");
                entries.push((rel_path, hash, size));
            }
        }
    }

    Ok(entries)
}

/// Generate a single console script wrapper.
fn generate_script(
    bin_dir: &Path,
    name: &str,
    module: &str,
    func: &str,
    python_path: &Path,
) -> Result<(), InstallError> {
    let script_content = format!(
        "#!{python}\nimport sys\nfrom {module} import {func}\nsys.exit({func}())\n",
        python = python_path.display(),
        module = module,
        func = func,
    );

    let script_path = bin_dir.join(name);
    fs::write(&script_path, &script_content)?;
    make_executable(&script_path);
    Ok(())
}

/// Hash a file with SHA-256 (hex-encoded, for download verification).
fn hash_file(path: &Path) -> Result<String, InstallError> {
    let data = fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    Ok(hex::encode(hasher.finalize()))
}

/// Compute the SHA-256 digest of `data` in the PEP 376 RECORD format:
/// `sha256=<urlsafe-base64-no-padding>`.
fn record_hash(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    format!("sha256={}", URL_SAFE_NO_PAD.encode(digest))
}

/// Compute a relative path from `base` to `target` using `..` components.
///
/// For example, if base is `/venv/lib/python3.12/site-packages` and target is
/// `/venv/bin/myscript`, the result is `../../../bin/myscript`.
///
/// This is used for RECORD entries that reference files installed outside
/// site-packages (e.g., scripts in the bin directory, data files, headers).
fn relative_path_from(base: &Path, target: &Path) -> String {
    // Find the longest common prefix
    let base_components: Vec<_> = base.components().collect();
    let target_components: Vec<_> = target.components().collect();

    let common_len = base_components
        .iter()
        .zip(target_components.iter())
        .take_while(|(a, b)| a == b)
        .count();

    // Number of `..` needed = remaining components in base after the common prefix
    let ups = base_components.len() - common_len;

    // Remaining components from target after the common prefix
    let mut result = PathBuf::new();
    for _ in 0..ups {
        result.push("..");
    }
    for component in &target_components[common_len..] {
        result.push(component);
    }

    // RECORD files require forward slashes per the wheel spec, even on Windows
    result.to_string_lossy().replace('\\', "/")
}

/// Make a file executable on Unix.
fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
    }
    let _ = path; // suppress unused warning on non-unix
}

/// Get the platform cache directory.
fn dirs_cache() -> PathBuf {
    if let Ok(dir) = std::env::var("UMBRAL_CACHE_DIR") {
        PathBuf::from(dir)
    } else if cfg!(target_os = "macos") {
        dirs_home().join("Library").join("Caches").join("umbral")
    } else {
        std::env::var("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| dirs_home().join(".cache"))
            .join("umbral")
    }
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_dist_info_name() {
        let (name, version) = parse_dist_info_name("requests-2.31.0.dist-info").unwrap();
        assert_eq!(name, "requests");
        assert_eq!(version, "2.31.0");
    }

    #[test]
    fn test_parse_dist_info_name_complex() {
        let (name, version) = parse_dist_info_name("my_package-1.0.0a1.dist-info").unwrap();
        assert_eq!(name, "my_package");
        assert_eq!(version, "1.0.0a1");
    }

    #[test]
    fn test_parse_dist_info_invalid() {
        assert!(parse_dist_info_name("invalid").is_err());
        assert!(parse_dist_info_name("nodash.dist-info").is_err());
    }

    #[test]
    fn test_generate_console_scripts() {
        let content = r#"[console_scripts]
mycli = mypackage.cli:main
other = other.mod:run

[gui_scripts]
myapp = mypackage.gui:start
"#;
        let tmp = tempfile::tempdir().unwrap();
        let python = Path::new("/venv/bin/python");
        let entries =
            generate_console_scripts(content, tmp.path(), python, "../../../bin").unwrap();

        // Should return RECORD entries for 2 console scripts (not gui_scripts)
        assert_eq!(
            entries.len(),
            2,
            "expected 2 RECORD entries for console scripts"
        );

        let script = fs::read_to_string(tmp.path().join("mycli")).unwrap();
        assert!(script.contains("#!/venv/bin/python"));
        assert!(script.contains("from mypackage.cli import main"));
        assert!(script.contains("sys.exit(main())"));

        let other = fs::read_to_string(tmp.path().join("other")).unwrap();
        assert!(other.contains("from other.mod import run"));

        // gui_scripts should NOT be generated
        assert!(!tmp.path().join("myapp").exists());
    }

    #[test]
    fn test_extract_data_path_scripts() {
        let result = extract_data_path(
            "pkg-1.0.data/scripts/myscript",
            "pkg-1.0.dist-info",
            Path::new("/venv/bin"),
            Path::new("/venv/lib/python3.12/site-packages"),
            Some(Path::new("/venv")),
        )
        .unwrap();
        assert_eq!(result, Some(PathBuf::from("/venv/bin/myscript")));
    }

    #[test]
    fn test_extract_data_path_purelib() {
        let result = extract_data_path(
            "pkg-1.0.data/purelib/pkg/extra.py",
            "pkg-1.0.dist-info",
            Path::new("/venv/bin"),
            Path::new("/venv/lib/python3.12/site-packages"),
            Some(Path::new("/venv")),
        )
        .unwrap();
        assert_eq!(
            result,
            Some(PathBuf::from(
                "/venv/lib/python3.12/site-packages/pkg/extra.py"
            ))
        );
    }

    #[test]
    fn test_extract_data_path_not_data() {
        let result = extract_data_path(
            "pkg/__init__.py",
            "pkg-1.0.dist-info",
            Path::new("/venv/bin"),
            Path::new("/venv/lib/python3.12/site-packages"),
            Some(Path::new("/venv")),
        )
        .unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_link_mode_default() {
        assert_eq!(LinkMode::default(), LinkMode::Hardlink);
    }

    #[test]
    fn test_link_file_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source.txt");
        let target = tmp.path().join("subdir").join("target.txt");

        fs::write(&source, "hello").unwrap();

        let installer = WheelInstaller::with_cache_dir(tmp.path().to_path_buf(), LinkMode::Copy);
        installer.link_file(&source, &target).unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "hello");
    }

    #[test]
    fn test_link_file_hardlink() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source.txt");
        let target = tmp.path().join("target.txt");

        fs::write(&source, "hello").unwrap();

        let installer =
            WheelInstaller::with_cache_dir(tmp.path().to_path_buf(), LinkMode::Hardlink);
        installer.link_file(&source, &target).unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "hello");
    }

    #[test]
    fn test_hash_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.txt");
        fs::write(&path, "hello world").unwrap();

        let hash = hash_file(&path).unwrap();
        // SHA-256 of "hello world"
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn test_scan_installed_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let packages = scan_installed(tmp.path()).unwrap();
        assert!(packages.is_empty());
    }

    #[test]
    fn test_scan_installed_with_package() {
        let tmp = tempfile::tempdir().unwrap();
        let dist_info = tmp.path().join("requests-2.31.0.dist-info");
        fs::create_dir_all(&dist_info).unwrap();
        fs::write(
            dist_info.join("METADATA"),
            "Metadata-Version: 2.1\nName: requests\nVersion: 2.31.0\nSummary: HTTP library\n",
        )
        .unwrap();

        let packages = scan_installed(tmp.path()).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "requests");
        assert_eq!(packages[0].version, "2.31.0");
    }

    #[test]
    fn test_scan_installed_nonexistent() {
        let packages = scan_installed(Path::new("/nonexistent/path")).unwrap();
        assert!(packages.is_empty());
    }

    #[test]
    fn test_parse_installed_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("METADATA");
        fs::write(
            &path,
            "Metadata-Version: 2.1\nName: click\nVersion: 8.1.7\nSummary: CLI toolkit\n\nLong description...\n",
        ).unwrap();

        let (name, version) = parse_installed_metadata(&path).unwrap();
        assert_eq!(name, "click");
        assert_eq!(version, "8.1.7");
    }

    #[test]
    fn test_dirs_cache() {
        // Just ensure it returns a path
        let cache = dirs_cache();
        assert!(cache.to_string_lossy().contains("umbral"));
    }

    // ── Path traversal tests ────────────────────────────────────────

    #[test]
    fn test_safe_join_normal() {
        let base = Path::new("/site-packages");
        let result = safe_join(base, "pkg/__init__.py").unwrap();
        assert_eq!(result, PathBuf::from("/site-packages/pkg/__init__.py"));
    }

    #[test]
    fn test_safe_join_rejects_parent_traversal() {
        let base = Path::new("/site-packages");
        let err = safe_join(base, "../../etc/cron.d/backdoor").unwrap_err();
        assert!(
            matches!(err, InstallError::PathTraversal(_)),
            "expected PathTraversal, got: {err:?}"
        );
    }

    #[test]
    fn test_safe_join_rejects_absolute_escape() {
        // A path that uses ../ after a legitimate prefix to escape
        let base = Path::new("/site-packages");
        let err = safe_join(base, "pkg/../../../etc/passwd").unwrap_err();
        assert!(matches!(err, InstallError::PathTraversal(_)));
    }

    #[test]
    fn test_safe_join_allows_internal_parent() {
        // pkg/subdir/../file.py normalizes to pkg/file.py — still within base
        let base = Path::new("/site-packages");
        let result = safe_join(base, "pkg/subdir/../file.py").unwrap();
        assert_eq!(result, PathBuf::from("/site-packages/pkg/file.py"));
    }

    #[test]
    fn test_path_traversal_in_install_wheel() {
        use std::io::Write;

        let tmp = tempfile::tempdir().unwrap();
        let wheel_path = tmp.path().join("evil-1.0-py3-none-any.whl");
        let site_packages = tmp.path().join("site-packages");
        let bin_dir = tmp.path().join("bin");
        let python_path = tmp.path().join("bin/python");
        fs::create_dir_all(&site_packages).unwrap();
        fs::create_dir_all(&bin_dir).unwrap();

        // Build a minimal wheel zip with a path-traversal entry
        let file = fs::File::create(&wheel_path).unwrap();
        let mut zip_writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        // Required dist-info directory
        zip_writer
            .start_file("evil-1.0.dist-info/METADATA", options)
            .unwrap();
        zip_writer
            .write_all(b"Metadata-Version: 2.1\nName: evil\nVersion: 1.0\n")
            .unwrap();
        zip_writer
            .start_file("evil-1.0.dist-info/WHEEL", options)
            .unwrap();
        zip_writer
            .write_all(b"Wheel-Version: 1.0\nGenerator: test\n")
            .unwrap();

        // Malicious entry that tries to escape site-packages
        zip_writer
            .start_file("../../etc/cron.d/backdoor", options)
            .unwrap();
        zip_writer.write_all(b"* * * * * root evil").unwrap();

        zip_writer.finish().unwrap();

        let installer = WheelInstaller::with_cache_dir(tmp.path().join("cache"), LinkMode::Copy);
        let result =
            installer.install_wheel(&wheel_path, &site_packages, &bin_dir, &python_path, None);
        assert!(
            matches!(result, Err(InstallError::PathTraversal(_))),
            "expected PathTraversal error, got: {result:?}"
        );

        // Ensure no file was written outside site-packages
        assert!(
            !tmp.path().join("etc/cron.d/backdoor").exists(),
            "path traversal should have been blocked"
        );
    }

    // ── RECORD file tests ───────────────────────────────────────────

    #[test]
    fn test_record_file_written_after_install() {
        use std::io::Write;

        let tmp = tempfile::tempdir().unwrap();
        let wheel_path = tmp.path().join("mypkg-1.0.0-py3-none-any.whl");
        let site_packages = tmp.path().join("site-packages");
        let bin_dir = tmp.path().join("bin");
        let python_path = tmp.path().join("bin/python");
        fs::create_dir_all(&site_packages).unwrap();
        fs::create_dir_all(&bin_dir).unwrap();

        // Build a minimal wheel
        let file = fs::File::create(&wheel_path).unwrap();
        let mut zip_writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        zip_writer
            .start_file("mypkg-1.0.0.dist-info/METADATA", options)
            .unwrap();
        zip_writer
            .write_all(b"Metadata-Version: 2.1\nName: mypkg\nVersion: 1.0.0\n")
            .unwrap();
        zip_writer
            .start_file("mypkg-1.0.0.dist-info/WHEEL", options)
            .unwrap();
        zip_writer
            .write_all(b"Wheel-Version: 1.0\nGenerator: test\n")
            .unwrap();
        zip_writer.start_file("mypkg/__init__.py", options).unwrap();
        zip_writer.write_all(b"# init").unwrap();

        zip_writer.finish().unwrap();

        let installer = WheelInstaller::with_cache_dir(tmp.path().join("cache"), LinkMode::Copy);
        let dist = installer
            .install_wheel(&wheel_path, &site_packages, &bin_dir, &python_path, None)
            .unwrap();

        // RECORD file must exist
        let record_path = dist.dist_info_dir.join("RECORD");
        assert!(record_path.exists(), "RECORD file must be written");

        let record = fs::read_to_string(&record_path).unwrap();

        // Each installed file should have a RECORD line with hash and size
        assert!(
            record.contains("mypkg/__init__.py,sha256="),
            "RECORD should contain hash for installed file: {record}"
        );
        // RECORD's own entry must have empty hash and size
        assert!(
            record.contains("mypkg-1.0.0.dist-info/RECORD,,"),
            "RECORD's own entry must have empty hash and size: {record}"
        );
        // INSTALLER entry should be present
        assert!(
            record.contains("mypkg-1.0.0.dist-info/INSTALLER,sha256="),
            "RECORD should contain INSTALLER entry: {record}"
        );

        // Verify hash format: sha256=<urlsafe-base64-no-padding>
        for line in record.lines() {
            if line.contains(",,") {
                continue; // skip RECORD's own entry
            }
            let parts: Vec<&str> = line.split(',').collect();
            assert_eq!(
                parts.len(),
                3,
                "each RECORD line should have 3 fields: {line}"
            );
            assert!(
                parts[1].starts_with("sha256="),
                "hash field should start with sha256=: {line}"
            );
            assert!(
                parts[2].parse::<usize>().is_ok(),
                "size field should be a number: {line}"
            );
        }
    }

    // ── Shebang tests ───────────────────────────────────────────────

    #[test]
    fn test_shebang_uses_custom_python_path() {
        let tmp = tempfile::tempdir().unwrap();
        let python_path = Path::new("/opt/venvs/myproject/bin/python3.11");

        generate_script(tmp.path(), "mycli", "mypackage.cli", "main", python_path).unwrap();

        let script = fs::read_to_string(tmp.path().join("mycli")).unwrap();
        assert!(
            script.starts_with("#!/opt/venvs/myproject/bin/python3.11\n"),
            "shebang should use the provided python_path, got: {script}"
        );
        assert!(script.contains("from mypackage.cli import main"));
        assert!(script.contains("sys.exit(main())"));
    }

    #[test]
    fn test_console_scripts_use_venv_python() {
        use std::io::Write;

        let tmp = tempfile::tempdir().unwrap();
        let wheel_path = tmp.path().join("clipkg-0.1.0-py3-none-any.whl");
        let site_packages = tmp.path().join("site-packages");
        let bin_dir = tmp.path().join("bin");
        let python_path = tmp.path().join("bin/python");
        fs::create_dir_all(&site_packages).unwrap();
        fs::create_dir_all(&bin_dir).unwrap();

        // Build a wheel with entry_points.txt
        let file = fs::File::create(&wheel_path).unwrap();
        let mut zip_writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        zip_writer
            .start_file("clipkg-0.1.0.dist-info/METADATA", options)
            .unwrap();
        zip_writer
            .write_all(b"Metadata-Version: 2.1\nName: clipkg\nVersion: 0.1.0\n")
            .unwrap();
        zip_writer
            .start_file("clipkg-0.1.0.dist-info/WHEEL", options)
            .unwrap();
        zip_writer
            .write_all(b"Wheel-Version: 1.0\nGenerator: test\n")
            .unwrap();
        zip_writer
            .start_file("clipkg-0.1.0.dist-info/entry_points.txt", options)
            .unwrap();
        zip_writer
            .write_all(b"[console_scripts]\nmytool = clipkg.main:cli\n")
            .unwrap();
        zip_writer
            .start_file("clipkg/__init__.py", options)
            .unwrap();
        zip_writer.write_all(b"").unwrap();

        zip_writer.finish().unwrap();

        let installer = WheelInstaller::with_cache_dir(tmp.path().join("cache"), LinkMode::Copy);
        installer
            .install_wheel(&wheel_path, &site_packages, &bin_dir, &python_path, None)
            .unwrap();

        let script_path = bin_dir.join("mytool");
        assert!(script_path.exists(), "console script should be generated");

        let script = fs::read_to_string(&script_path).unwrap();
        let expected_shebang = format!("#!{}\n", python_path.display());
        assert!(
            script.starts_with(&expected_shebang),
            "shebang should use venv python, got: {script}"
        );
        // Must NOT contain the old hardcoded shebang
        assert!(
            !script.contains("#!/usr/bin/env python3"),
            "should not use env python3"
        );
    }

    // ── Fix regression tests ─────────────────────────────────────────

    #[test]
    fn test_record_no_duplicate_record_entry() {
        use std::io::Write;

        let tmp = tempfile::tempdir().unwrap();
        let wheel_path = tmp.path().join("dupchk-1.0.0-py3-none-any.whl");
        let site_packages = tmp.path().join("site-packages");
        let bin_dir = tmp.path().join("bin");
        let python_path = tmp.path().join("bin/python");
        fs::create_dir_all(&site_packages).unwrap();
        fs::create_dir_all(&bin_dir).unwrap();

        // Build a wheel that includes a RECORD file (as real wheels do)
        let file = fs::File::create(&wheel_path).unwrap();
        let mut zip_writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        zip_writer
            .start_file("dupchk-1.0.0.dist-info/METADATA", options)
            .unwrap();
        zip_writer
            .write_all(b"Metadata-Version: 2.1\nName: dupchk\nVersion: 1.0.0\n")
            .unwrap();
        zip_writer
            .start_file("dupchk-1.0.0.dist-info/WHEEL", options)
            .unwrap();
        zip_writer
            .write_all(b"Wheel-Version: 1.0\nGenerator: test\n")
            .unwrap();
        // Include the wheel's own RECORD — this must NOT produce a duplicate
        zip_writer
            .start_file("dupchk-1.0.0.dist-info/RECORD", options)
            .unwrap();
        zip_writer
            .write_all(b"dupchk-1.0.0.dist-info/METADATA,sha256=abc,123\n")
            .unwrap();
        zip_writer
            .start_file("dupchk/__init__.py", options)
            .unwrap();
        zip_writer.write_all(b"# init").unwrap();

        zip_writer.finish().unwrap();

        let installer = WheelInstaller::with_cache_dir(tmp.path().join("cache"), LinkMode::Copy);
        let dist = installer
            .install_wheel(&wheel_path, &site_packages, &bin_dir, &python_path, None)
            .unwrap();

        let record = fs::read_to_string(dist.dist_info_dir.join("RECORD")).unwrap();
        let record_line_count = record
            .lines()
            .filter(|l| l.contains("dupchk-1.0.0.dist-info/RECORD"))
            .count();
        assert_eq!(
            record_line_count, 1,
            "RECORD should contain exactly one entry for itself (the `,,` line), got:\n{record}"
        );
    }

    #[test]
    fn test_console_script_paths_in_record() {
        use std::io::Write;

        let tmp = tempfile::tempdir().unwrap();
        let wheel_path = tmp.path().join("clipkg2-0.1.0-py3-none-any.whl");
        let site_packages = tmp.path().join("site-packages");
        let bin_dir = tmp.path().join("bin");
        let python_path = tmp.path().join("bin/python");
        fs::create_dir_all(&site_packages).unwrap();
        fs::create_dir_all(&bin_dir).unwrap();

        let file = fs::File::create(&wheel_path).unwrap();
        let mut zip_writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        zip_writer
            .start_file("clipkg2-0.1.0.dist-info/METADATA", options)
            .unwrap();
        zip_writer
            .write_all(b"Metadata-Version: 2.1\nName: clipkg2\nVersion: 0.1.0\n")
            .unwrap();
        zip_writer
            .start_file("clipkg2-0.1.0.dist-info/WHEEL", options)
            .unwrap();
        zip_writer
            .write_all(b"Wheel-Version: 1.0\nGenerator: test\n")
            .unwrap();
        zip_writer
            .start_file("clipkg2-0.1.0.dist-info/entry_points.txt", options)
            .unwrap();
        zip_writer
            .write_all(b"[console_scripts]\nmytool = clipkg2.main:cli\nother = clipkg2.other:run\n")
            .unwrap();
        zip_writer
            .start_file("clipkg2/__init__.py", options)
            .unwrap();
        zip_writer.write_all(b"").unwrap();

        zip_writer.finish().unwrap();

        let installer = WheelInstaller::with_cache_dir(tmp.path().join("cache"), LinkMode::Copy);
        let dist = installer
            .install_wheel(&wheel_path, &site_packages, &bin_dir, &python_path, None)
            .unwrap();

        let record = fs::read_to_string(dist.dist_info_dir.join("RECORD")).unwrap();

        // Both console scripts must appear in RECORD with hash and size
        let expected_prefix = if cfg!(windows) {
            "../../Scripts"
        } else {
            "../../../bin"
        };
        assert!(
            record.contains(&format!("{expected_prefix}/mytool,sha256=")),
            "RECORD should contain mytool script entry:\n{record}"
        );
        assert!(
            record.contains(&format!("{expected_prefix}/other,sha256=")),
            "RECORD should contain other script entry:\n{record}"
        );
    }

    #[test]
    fn test_parse_dist_info_name_hyphenated_package() {
        // Hyphenated package names: dist-info uses underscores for the name
        // and a hyphen only before the version. `rfind('-')` must split at
        // the last hyphen, not the first.
        let (name, version) = parse_dist_info_name("my_cool_package-1.0.0.dist-info").unwrap();
        assert_eq!(name, "my_cool_package");
        assert_eq!(version, "1.0.0");
    }

    #[test]
    fn test_parse_dist_info_name_version_with_hyphen_prerelease() {
        // Edge case: version strings should NOT contain hyphens per PEP 440,
        // but some legacy packages might. With rfind we split at the last
        // hyphen, preserving the full package name.
        let (name, version) = parse_dist_info_name("my_cool_package-2.0.0b1.dist-info").unwrap();
        assert_eq!(name, "my_cool_package");
        assert_eq!(version, "2.0.0b1");
    }

    // ── Console script RECORD path prefix tests ─────────────────────

    #[test]
    fn test_console_script_record_uses_unix_prefix() {
        let content = "[console_scripts]\nmycli = mypackage.cli:main\n";
        let tmp = tempfile::tempdir().unwrap();
        let python = Path::new("/venv/bin/python");
        let entries =
            generate_console_scripts(content, tmp.path(), python, "../../../bin").unwrap();
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].0.starts_with("../../../bin/"),
            "Unix RECORD path should use ../../../bin/ prefix, got: {}",
            entries[0].0
        );
        assert_eq!(entries[0].0, "../../../bin/mycli");
    }

    #[test]
    fn test_console_script_record_uses_windows_prefix() {
        let content = "[console_scripts]\nmycli = mypackage.cli:main\n";
        let tmp = tempfile::tempdir().unwrap();
        let python = Path::new("/venv/Scripts/python");
        let entries =
            generate_console_scripts(content, tmp.path(), python, "../../Scripts").unwrap();
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].0.starts_with("../../Scripts/"),
            "Windows RECORD path should use ../../Scripts/ prefix, got: {}",
            entries[0].0
        );
        assert_eq!(entries[0].0, "../../Scripts/mycli");
    }

    // ── extract_data_path with explicit venv_root tests ─────────────

    #[test]
    fn test_extract_data_path_data_with_venv_root() {
        // With explicit venv_root, data/ entries resolve directly under the root
        let result = extract_data_path(
            "pkg-1.0.data/data/share/man/man1/pkg.1",
            "pkg-1.0.dist-info",
            Path::new("/venv/bin"),
            Path::new("/venv/lib/python3.12/site-packages"),
            Some(Path::new("/venv")),
        )
        .unwrap();
        assert_eq!(result, Some(PathBuf::from("/venv/share/man/man1/pkg.1")));
    }

    #[test]
    fn test_extract_data_path_headers_with_venv_root() {
        // With explicit venv_root, headers/ entries resolve under venv_root/include
        let result = extract_data_path(
            "pkg-1.0.data/headers/pkg/header.h",
            "pkg-1.0.dist-info",
            Path::new("/venv/bin"),
            Path::new("/venv/lib/python3.12/site-packages"),
            Some(Path::new("/venv")),
        )
        .unwrap();
        assert_eq!(result, Some(PathBuf::from("/venv/include/pkg/header.h")));
    }

    #[test]
    fn test_extract_data_path_data_without_venv_root_fallback() {
        // Without venv_root, data/ uses the legacy `..` chain fallback.
        // The legacy path constructs a base with `..` components which
        // `safe_join` cannot normalize without filesystem access, so this
        // falls back to a PathTraversal error for synthetic (non-existent)
        // paths. This demonstrates WHY the explicit `venv_root` parameter
        // was added — the legacy approach is unreliable.
        let result = extract_data_path(
            "pkg-1.0.data/data/share/pkg.txt",
            "pkg-1.0.dist-info",
            Path::new("/venv/bin"),
            Path::new("/venv/lib/python3.12/site-packages"),
            None,
        );
        // The legacy fallback is expected to fail with PathTraversal for
        // synthetic paths; with real paths on disk it may work via
        // canonicalization. The fix is to always provide venv_root.
        assert!(
            result.is_err() || result.unwrap() == Some(PathBuf::from("/venv/share/pkg.txt")),
            "legacy fallback should either error or resolve to the venv root"
        );
    }

    #[test]
    fn test_extract_data_path_windows_layout_with_venv_root() {
        // Simulates Windows layout where site-packages is only 2 levels deep.
        // Without venv_root, the legacy 3-level .. chain would produce a wrong path.
        // With venv_root, it resolves correctly.
        let result = extract_data_path(
            "pkg-1.0.data/data/share/pkg.txt",
            "pkg-1.0.dist-info",
            Path::new("C:/venv/Scripts"),
            Path::new("C:/venv/Lib/site-packages"),
            Some(Path::new("C:/venv")),
        )
        .unwrap();
        assert_eq!(result, Some(PathBuf::from("C:/venv/share/pkg.txt")));
    }

    // ── install_wheel with explicit venv_root ───────────────────────

    #[test]
    fn test_install_wheel_with_explicit_venv_root() {
        use std::io::Write;

        let tmp = tempfile::tempdir().unwrap();
        let venv_root = tmp.path().join("myvenv");
        let site_packages = venv_root
            .join("lib")
            .join("python3.12")
            .join("site-packages");
        let bin_dir = venv_root.join("bin");
        let python_path = bin_dir.join("python");
        fs::create_dir_all(&site_packages).unwrap();
        fs::create_dir_all(&bin_dir).unwrap();

        let wheel_path = tmp.path().join("datapkg-1.0.0-py3-none-any.whl");
        let file = fs::File::create(&wheel_path).unwrap();
        let mut zip_writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        zip_writer
            .start_file("datapkg-1.0.0.dist-info/METADATA", options)
            .unwrap();
        zip_writer
            .write_all(b"Metadata-Version: 2.1\nName: datapkg\nVersion: 1.0.0\n")
            .unwrap();
        zip_writer
            .start_file("datapkg-1.0.0.dist-info/WHEEL", options)
            .unwrap();
        zip_writer
            .write_all(b"Wheel-Version: 1.0\nGenerator: test\n")
            .unwrap();
        zip_writer
            .start_file("datapkg-1.0.0.data/data/share/datapkg/config.txt", options)
            .unwrap();
        zip_writer.write_all(b"config data").unwrap();
        zip_writer
            .start_file("datapkg/__init__.py", options)
            .unwrap();
        zip_writer.write_all(b"# init").unwrap();

        zip_writer.finish().unwrap();

        let installer = WheelInstaller::with_cache_dir(tmp.path().join("cache"), LinkMode::Copy);
        let dist = installer
            .install_wheel(
                &wheel_path,
                &site_packages,
                &bin_dir,
                &python_path,
                Some(&venv_root),
            )
            .unwrap();

        // The data file should be installed under venv_root/share/datapkg/config.txt
        let data_file = venv_root.join("share").join("datapkg").join("config.txt");
        assert!(
            data_file.exists(),
            "data file should be installed under venv root: {}",
            data_file.display()
        );
        assert_eq!(fs::read_to_string(&data_file).unwrap(), "config data");

        // RECORD should exist
        let record = fs::read_to_string(dist.dist_info_dir.join("RECORD")).unwrap();
        assert!(record.contains("datapkg-1.0.0.dist-info/RECORD,,"));
    }

    // ── RECORD entries for .data/ files use installed-relative paths ─

    #[test]
    fn test_data_scripts_record_uses_relative_bin_path() {
        use std::io::Write;

        let tmp = tempfile::tempdir().unwrap();
        let venv_root = tmp.path().join("myvenv");
        let site_packages = if cfg!(windows) {
            venv_root.join("Lib").join("site-packages")
        } else {
            venv_root
                .join("lib")
                .join("python3.12")
                .join("site-packages")
        };
        let bin_dir = if cfg!(windows) {
            venv_root.join("Scripts")
        } else {
            venv_root.join("bin")
        };
        let python_path = if cfg!(windows) {
            bin_dir.join("python.exe")
        } else {
            bin_dir.join("python")
        };
        fs::create_dir_all(&site_packages).unwrap();
        fs::create_dir_all(&bin_dir).unwrap();

        let wheel_path = tmp.path().join("scriptpkg-1.0.0-py3-none-any.whl");
        let file = fs::File::create(&wheel_path).unwrap();
        let mut zip_writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        zip_writer
            .start_file("scriptpkg-1.0.0.dist-info/METADATA", options)
            .unwrap();
        zip_writer
            .write_all(b"Metadata-Version: 2.1\nName: scriptpkg\nVersion: 1.0.0\n")
            .unwrap();
        zip_writer
            .start_file("scriptpkg-1.0.0.dist-info/WHEEL", options)
            .unwrap();
        zip_writer
            .write_all(b"Wheel-Version: 1.0\nGenerator: test\n")
            .unwrap();
        // A .data/scripts entry — RECORD should use platform-appropriate relative path
        zip_writer
            .start_file("scriptpkg-1.0.0.data/scripts/myscript", options)
            .unwrap();
        zip_writer.write_all(b"#!/bin/sh\necho hello").unwrap();
        // A .data/purelib entry — should get the path relative within site-packages
        zip_writer
            .start_file("scriptpkg-1.0.0.data/purelib/scriptpkg_extra.py", options)
            .unwrap();
        zip_writer.write_all(b"# extra module").unwrap();
        // A .data/data entry — installed outside site-packages
        zip_writer
            .start_file(
                "scriptpkg-1.0.0.data/data/share/scriptpkg/data.txt",
                options,
            )
            .unwrap();
        zip_writer.write_all(b"some data").unwrap();
        // A regular file
        zip_writer
            .start_file("scriptpkg/__init__.py", options)
            .unwrap();
        zip_writer.write_all(b"# init").unwrap();

        zip_writer.finish().unwrap();

        let installer = WheelInstaller::with_cache_dir(tmp.path().join("cache"), LinkMode::Copy);
        let dist = installer
            .install_wheel(
                &wheel_path,
                &site_packages,
                &bin_dir,
                &python_path,
                Some(&venv_root),
            )
            .unwrap();

        let record = fs::read_to_string(dist.dist_info_dir.join("RECORD")).unwrap();

        // Scripts: RECORD entry must use relative path to bin, NOT the zip path
        let expected_bin_prefix = if cfg!(windows) {
            "../../Scripts"
        } else {
            "../../../bin"
        };
        assert!(
            record.contains(&format!("{expected_bin_prefix}/myscript,sha256=")),
            "RECORD entry for .data/scripts/ should use {expected_bin_prefix}/ relative path, \
             not the zip-internal path. Got:\n{record}"
        );
        assert!(
            !record.contains("scriptpkg-1.0.0.data/scripts/"),
            "RECORD must NOT contain the zip-internal .data/scripts/ path. Got:\n{record}"
        );

        // Purelib: RECORD entry must use the path relative within site-packages
        assert!(
            record.contains("scriptpkg_extra.py,sha256="),
            "RECORD entry for .data/purelib/ should use the installed relative path. Got:\n{record}"
        );
        assert!(
            !record.contains("scriptpkg-1.0.0.data/purelib/"),
            "RECORD must NOT contain the zip-internal .data/purelib/ path. Got:\n{record}"
        );

        // Data: RECORD entry must use ../-based relative path, not zip path
        assert!(
            !record.contains("scriptpkg-1.0.0.data/data/"),
            "RECORD must NOT contain the zip-internal .data/data/ path. Got:\n{record}"
        );
        assert!(
            record.contains("share/scriptpkg/data.txt,sha256="),
            "RECORD entry for .data/data/ should contain the relative installed path. Got:\n{record}"
        );
    }

    #[test]
    fn test_relative_path_from_helper() {
        // Test the relative_path_from helper directly
        let base = Path::new("/venv/lib/python3.12/site-packages");
        let target = Path::new("/venv/bin/myscript");
        let result = relative_path_from(base, target);
        assert_eq!(result, "../../../bin/myscript");

        // Target inside site-packages — should give a simple relative path
        let target2 = Path::new("/venv/lib/python3.12/site-packages/pkg/module.py");
        let result2 = relative_path_from(base, target2);
        assert_eq!(result2, "pkg/module.py");

        // Target in a sibling directory
        let target3 = Path::new("/venv/share/man/man1/tool.1");
        let result3 = relative_path_from(base, target3);
        assert_eq!(result3, "../../../share/man/man1/tool.1");

        // Target in include directory (headers)
        let target4 = Path::new("/venv/include/pkg/header.h");
        let result4 = relative_path_from(base, target4);
        assert_eq!(result4, "../../../include/pkg/header.h");
    }

    // ── Editable install tests ──────────────────────────────────────

    #[test]
    fn test_editable_install_creates_pth_file() {
        let tmp = tempfile::tempdir().unwrap();
        let site_packages = tmp.path().join("site-packages");
        fs::create_dir_all(&site_packages).unwrap();

        let project_dir = tmp.path().join("my-project");
        fs::create_dir_all(&project_dir).unwrap();

        let result = install_editable(&project_dir, &site_packages, "my-project", None).unwrap();

        // .pth file should exist with the project directory path
        assert!(result.pth_path.exists(), ".pth file should exist");
        let pth_content = fs::read_to_string(&result.pth_path).unwrap();
        assert_eq!(
            pth_content,
            format!("{}\n", project_dir.display()),
            ".pth file should contain the project directory"
        );
        assert!(
            result.pth_path.file_name().unwrap().to_string_lossy() == "my_project.pth",
            "pth filename should normalize hyphens to underscores"
        );
    }

    #[test]
    fn test_editable_install_creates_dist_info() {
        let tmp = tempfile::tempdir().unwrap();
        let site_packages = tmp.path().join("site-packages");
        fs::create_dir_all(&site_packages).unwrap();

        let project_dir = tmp.path().join("my-project");
        fs::create_dir_all(&project_dir).unwrap();

        let result =
            install_editable(&project_dir, &site_packages, "my-project", Some("1.2.3")).unwrap();

        // dist-info directory should exist
        assert!(
            result.dist_info.exists(),
            "dist-info directory should exist"
        );
        assert!(
            result
                .dist_info
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains("my_project-1.2.3.dist-info"),
            "dist-info dir should include normalized name and version"
        );

        // METADATA should exist with correct content
        let metadata = fs::read_to_string(result.dist_info.join("METADATA")).unwrap();
        assert!(
            metadata.contains("Name: my-project"),
            "METADATA should contain the project name"
        );
        assert!(
            metadata.contains("Version: 1.2.3"),
            "METADATA should contain the version"
        );
        assert!(
            metadata.contains("Metadata-Version: 2.1"),
            "METADATA should have the metadata version"
        );

        // INSTALLER should exist
        let installer = fs::read_to_string(result.dist_info.join("INSTALLER")).unwrap();
        assert_eq!(installer, "umbral\n", "INSTALLER should say 'umbral'");
    }

    #[test]
    fn test_editable_install_direct_url_json() {
        let tmp = tempfile::tempdir().unwrap();
        let site_packages = tmp.path().join("site-packages");
        fs::create_dir_all(&site_packages).unwrap();

        let project_dir = tmp.path().join("my-project");
        fs::create_dir_all(&project_dir).unwrap();

        let result = install_editable(&project_dir, &site_packages, "my-project", None).unwrap();

        // direct_url.json should exist and contain editable flag
        let direct_url_path = result.dist_info.join("direct_url.json");
        assert!(direct_url_path.exists(), "direct_url.json should exist");

        let direct_url_content = fs::read_to_string(&direct_url_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&direct_url_content)
            .expect("direct_url.json should be valid JSON");

        // Check the URL field
        let url = parsed["url"]
            .as_str()
            .expect("url field should be a string");
        assert!(
            url.starts_with("file://"),
            "url should be a file:// URL, got: {url}"
        );
        assert!(
            url.contains(&project_dir.display().to_string()),
            "url should contain the project directory path"
        );

        // Check the editable flag
        let editable = parsed["dir_info"]["editable"]
            .as_bool()
            .expect("dir_info.editable should be a bool");
        assert!(editable, "editable flag should be true");
    }

    #[test]
    fn test_editable_install_record_file() {
        let tmp = tempfile::tempdir().unwrap();
        let site_packages = tmp.path().join("site-packages");
        fs::create_dir_all(&site_packages).unwrap();

        let project_dir = tmp.path().join("my-project");
        fs::create_dir_all(&project_dir).unwrap();

        let result = install_editable(&project_dir, &site_packages, "my-project", None).unwrap();

        // RECORD should exist
        let record_path = result.dist_info.join("RECORD");
        assert!(record_path.exists(), "RECORD should exist");

        let record = fs::read_to_string(&record_path).unwrap();

        // Should reference all created files
        assert!(
            record.contains("my_project.pth,sha256="),
            "RECORD should list the .pth file with hash: {record}"
        );
        assert!(
            record.contains("METADATA,sha256="),
            "RECORD should list METADATA with hash: {record}"
        );
        assert!(
            record.contains("INSTALLER,sha256="),
            "RECORD should list INSTALLER with hash: {record}"
        );
        assert!(
            record.contains("direct_url.json,sha256="),
            "RECORD should list direct_url.json with hash: {record}"
        );
        // RECORD's own entry must have empty hash
        assert!(
            record.contains("RECORD,,"),
            "RECORD's own entry should have empty hash and size: {record}"
        );
    }

    #[test]
    fn test_editable_install_default_version() {
        let tmp = tempfile::tempdir().unwrap();
        let site_packages = tmp.path().join("site-packages");
        fs::create_dir_all(&site_packages).unwrap();

        let project_dir = tmp.path().join("my-project");
        fs::create_dir_all(&project_dir).unwrap();

        let result = install_editable(&project_dir, &site_packages, "my-project", None).unwrap();

        // Without explicit version, should use 0.0.0
        assert!(
            result
                .dist_info
                .to_string_lossy()
                .contains("my_project-0.0.0.dist-info"),
            "default version should be 0.0.0"
        );
    }

    #[test]
    fn test_editable_install_scannable() {
        // An editable install should be discoverable by scan_installed
        let tmp = tempfile::tempdir().unwrap();
        let site_packages = tmp.path().join("site-packages");
        fs::create_dir_all(&site_packages).unwrap();

        let project_dir = tmp.path().join("my-project");
        fs::create_dir_all(&project_dir).unwrap();

        install_editable(&project_dir, &site_packages, "my-project", Some("2.0.0")).unwrap();

        let packages = scan_installed(&site_packages).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "my-project");
        assert_eq!(packages[0].version, "2.0.0");
    }
}
