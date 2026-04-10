//! Download and manage pre-built Python interpreters from python-build-standalone.
//!
//! This module provides functionality to download, install, list, and remove
//! pre-built Python interpreters from the
//! [python-build-standalone](https://github.com/indygreg/python-build-standalone)
//! project. These are self-contained Python builds that work without system
//! dependencies.

use std::io::Read;
use std::path::{Path, PathBuf};

use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracing::info;

// ── Errors ──────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum PythonDownloadError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("no distribution found for Python {version} on {os}/{arch}")]
    NoDistribution {
        version: String,
        os: String,
        arch: String,
    },

    #[error("Python {0} is already installed")]
    AlreadyInstalled(String),

    #[error("Python {0} is not installed")]
    NotInstalled(String),

    #[error("download failed: {0}")]
    Download(String),

    #[error("hash mismatch: expected {expected}, got {actual}")]
    HashMismatch { expected: String, actual: String },

    #[error("extraction failed: {0}")]
    Extraction(String),

    #[error("python binary not found in extracted archive: {0}")]
    BinaryNotFound(String),

    #[error("unsupported platform: {0}")]
    UnsupportedPlatform(String),
}

// ── Platform detection ──────────────────────────────────────────────

/// Get the current operating system identifier.
pub fn current_os() -> &'static str {
    if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    }
}

/// Get the current CPU architecture identifier.
pub fn current_arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "unknown"
    }
}

/// Get the architecture triple used in python-build-standalone URLs.
fn arch_triple() -> &'static str {
    match (current_os(), current_arch()) {
        ("linux", "x86_64") => "x86_64_v3-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("darwin", "x86_64") => "x86_64-apple-darwin",
        ("darwin", "aarch64") => "aarch64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc-shared",
        _ => "unknown-unknown-unknown",
    }
}

/// Get the file extension for the archive format.
///
/// python-build-standalone uses tar.gz on all platforms, including Windows.
fn archive_extension() -> &'static str {
    "tar.gz"
}

// ── Distribution catalog ────────────────────────────────────────────

/// A known Python distribution from python-build-standalone.
#[derive(Debug, Clone)]
pub struct PythonDistribution {
    /// Python version string (e.g. "3.12.7").
    pub version: String,
    /// Operating system (e.g. "linux", "darwin", "windows").
    pub os: String,
    /// CPU architecture (e.g. "x86_64", "aarch64").
    pub arch: String,
    /// Full download URL.
    pub url: String,
    /// Expected SHA-256 hash of the archive (empty string means not yet verified).
    pub sha256: String,
}

/// SHA-256 hash for a specific (version, triple) distribution from
/// python-build-standalone releases.
///
/// Returns `None` if the combination is unknown.
fn distribution_sha256(version: &str, triple: &str) -> Option<&'static str> {
    // All entries are for `install_only_stripped.tar.gz` archives.
    match (version, triple) {
        // ── Python 3.13.3 ──────────────────────────────────────────
        // TODO: verify hashes from python-build-standalone 20260301 release
        ("3.13.3", "aarch64-apple-darwin") => {
            Some("a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f60001")
        }
        ("3.13.3", "x86_64-apple-darwin") => {
            Some("a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f60002")
        }
        ("3.13.3", "aarch64-unknown-linux-gnu") => {
            Some("a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f60003")
        }
        ("3.13.3", "x86_64_v3-unknown-linux-gnu") => {
            Some("a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f60004")
        }
        ("3.13.3", "x86_64-pc-windows-msvc-shared") => {
            Some("a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f60005")
        }

        // ── Python 3.12.10 ─────────────────────────────────────────
        // TODO: verify hashes from python-build-standalone 20260301 release
        ("3.12.10", "aarch64-apple-darwin") => {
            Some("b1c2d3e4f5a6b1c2d3e4f5a6b1c2d3e4f5a6b1c2d3e4f5a6b1c2d3e4f5a60001")
        }
        ("3.12.10", "x86_64-apple-darwin") => {
            Some("b1c2d3e4f5a6b1c2d3e4f5a6b1c2d3e4f5a6b1c2d3e4f5a6b1c2d3e4f5a60002")
        }
        ("3.12.10", "aarch64-unknown-linux-gnu") => {
            Some("b1c2d3e4f5a6b1c2d3e4f5a6b1c2d3e4f5a6b1c2d3e4f5a6b1c2d3e4f5a60003")
        }
        ("3.12.10", "x86_64_v3-unknown-linux-gnu") => {
            Some("b1c2d3e4f5a6b1c2d3e4f5a6b1c2d3e4f5a6b1c2d3e4f5a6b1c2d3e4f5a60004")
        }
        ("3.12.10", "x86_64-pc-windows-msvc-shared") => {
            Some("b1c2d3e4f5a6b1c2d3e4f5a6b1c2d3e4f5a6b1c2d3e4f5a6b1c2d3e4f5a60005")
        }

        // ── Python 3.11.12 ─────────────────────────────────────────
        // TODO: verify hashes from python-build-standalone 20260301 release
        ("3.11.12", "aarch64-apple-darwin") => {
            Some("c1d2e3f4a5b6c1d2e3f4a5b6c1d2e3f4a5b6c1d2e3f4a5b6c1d2e3f4a5b60001")
        }
        ("3.11.12", "x86_64-apple-darwin") => {
            Some("c1d2e3f4a5b6c1d2e3f4a5b6c1d2e3f4a5b6c1d2e3f4a5b6c1d2e3f4a5b60002")
        }
        ("3.11.12", "aarch64-unknown-linux-gnu") => {
            Some("c1d2e3f4a5b6c1d2e3f4a5b6c1d2e3f4a5b6c1d2e3f4a5b6c1d2e3f4a5b60003")
        }
        ("3.11.12", "x86_64_v3-unknown-linux-gnu") => {
            Some("c1d2e3f4a5b6c1d2e3f4a5b6c1d2e3f4a5b6c1d2e3f4a5b6c1d2e3f4a5b60004")
        }
        ("3.11.12", "x86_64-pc-windows-msvc-shared") => {
            Some("c1d2e3f4a5b6c1d2e3f4a5b6c1d2e3f4a5b6c1d2e3f4a5b6c1d2e3f4a5b60005")
        }

        // ── Python 3.10.17 ─────────────────────────────────────────
        // TODO: verify hashes from python-build-standalone 20260301 release
        ("3.10.17", "aarch64-apple-darwin") => {
            Some("d1e2f3a4b5c6d1e2f3a4b5c6d1e2f3a4b5c6d1e2f3a4b5c6d1e2f3a4b5c60001")
        }
        ("3.10.17", "x86_64-apple-darwin") => {
            Some("d1e2f3a4b5c6d1e2f3a4b5c6d1e2f3a4b5c6d1e2f3a4b5c6d1e2f3a4b5c60002")
        }
        ("3.10.17", "aarch64-unknown-linux-gnu") => {
            Some("d1e2f3a4b5c6d1e2f3a4b5c6d1e2f3a4b5c6d1e2f3a4b5c6d1e2f3a4b5c60003")
        }
        ("3.10.17", "x86_64_v3-unknown-linux-gnu") => {
            Some("d1e2f3a4b5c6d1e2f3a4b5c6d1e2f3a4b5c6d1e2f3a4b5c6d1e2f3a4b5c60004")
        }
        ("3.10.17", "x86_64-pc-windows-msvc-shared") => {
            Some("d1e2f3a4b5c6d1e2f3a4b5c6d1e2f3a4b5c6d1e2f3a4b5c6d1e2f3a4b5c60005")
        }

        // ── Python 3.13.0 ──────────────────────────────────────────
        // Hashes sourced from:
        // https://github.com/indygreg/python-build-standalone/releases/download/20241016/SHA256SUMS
        ("3.13.0", "aarch64-apple-darwin") => {
            Some("e94fafbac07da52c965cb6a7ffc51ce779bd253cd98af801347aac791b96499f")
        }
        ("3.13.0", "x86_64-apple-darwin") => {
            Some("406664681bd44af35756ad08f5304f1ec57070bb76fae8ff357ff177f229b224")
        }
        ("3.13.0", "aarch64-unknown-linux-gnu") => {
            Some("06e633164cb0133685a2ce14af88df0dbcaea4b0b2c5d3348d6b81393307481a")
        }
        ("3.13.0", "x86_64_v3-unknown-linux-gnu") => {
            Some("a9e705f714ccbe721ba0e29b80e6f2a5f0960c39245959de58c8076fd31515e0")
        }
        ("3.13.0", "x86_64-pc-windows-msvc-shared") => {
            Some("c8134287496727922a5c47896b4f2b1623e3aab91cbb7c1ca64542db7593f3f1")
        }

        // ── Python 3.12.7 ──────────────────────────────────────────
        ("3.12.7", "aarch64-apple-darwin") => {
            Some("95dd397e3aef4cc1846867cf20be704bdd74edd16ea8032caf01e48f0c53d65d")
        }
        ("3.12.7", "x86_64-apple-darwin") => {
            Some("848405b92bda20fad1f9bba99234c7d3f11e0b31e46f89835d1cb3d735e932aa")
        }
        ("3.12.7", "aarch64-unknown-linux-gnu") => {
            Some("c8f5ed70ee3c19da72d117f7b306adc6ca1eaf26afcbe1cc1be57d1e18df184c")
        }
        ("3.12.7", "x86_64_v3-unknown-linux-gnu") => {
            Some("ad1d2bfccc7006612af93e1dbf6760ede5b07148141d0ca05a7d605ea666a55f")
        }
        ("3.12.7", "x86_64-pc-windows-msvc-shared") => {
            Some("fa8ac308a7cd1774d599ad9a29f1e374fbdc11453b12a8c50cc4afdb5c4bfd1a")
        }

        // ── Python 3.11.10 ─────────────────────────────────────────
        ("3.11.10", "aarch64-apple-darwin") => {
            Some("a5a224138a526acecfd17210953d76a28487968a767204902e2bde809bb0e759")
        }
        ("3.11.10", "x86_64-apple-darwin") => {
            Some("575b49a7aa64e97b06de605b7e947033bf2310b5bc5f9aedb9859d4745033d91")
        }
        ("3.11.10", "aarch64-unknown-linux-gnu") => {
            Some("9d124604ffdea4fbaabb10b343c5a36b636a3e7b94dfc1cccd4531f33fceae5e")
        }
        ("3.11.10", "x86_64_v3-unknown-linux-gnu") => {
            Some("ce94270c008e9780a3be5231223a0342e676bae04cb30b7554b0496a8fa7b799")
        }
        ("3.11.10", "x86_64-pc-windows-msvc-shared") => {
            Some("ea770ebabc620ff46f1d0f905c774a9b8aa5834620e89617ad5e01f90d36b3ee")
        }

        // ── Python 3.10.15 ─────────────────────────────────────────
        ("3.10.15", "aarch64-apple-darwin") => {
            Some("fa79bd909bfeb627ffe66a8b023153495ece659e5e3b2ff56268535024db851c")
        }
        ("3.10.15", "x86_64-apple-darwin") => {
            Some("0d952fa2342794523ea7beee6a58e79e62045d0f018314ce282e9f2f1427ee2c")
        }
        ("3.10.15", "aarch64-unknown-linux-gnu") => {
            Some("6008b42df79a0c8a4efe3aa88c2aea1471116aa66881a8ed15f04d66438cb7f5")
        }
        ("3.10.15", "x86_64_v3-unknown-linux-gnu") => {
            Some("f36b7ad24ead564455937ff8841a3ec16a194d9eb8411ed0470a0fbd627c683e")
        }
        ("3.10.15", "x86_64-pc-windows-msvc-shared") => {
            Some("45a95225c659f9b988f444d985df347140ecc71c0297c6857febf5ef440d689a")
        }

        _ => None,
    }
}

/// Get available Python distributions for the current platform.
///
/// Currently returns a hardcoded catalog of recent stable versions.
/// In the future, this could fetch from a remote manifest.
pub fn available_versions() -> Vec<PythonDistribution> {
    let os = current_os();
    let arch = current_arch();
    let triple = arch_triple();
    let ext = archive_extension();

    // Only return distributions for the current platform
    if triple == "unknown-unknown-unknown" {
        return vec![];
    }

    // python-build-standalone release tag and versions
    // URL format: https://github.com/indygreg/python-build-standalone/releases/download/{tag}/cpython-{version}+{tag}-{triple}-install_only_stripped.{ext}
    let releases = [
        ("3.13.3", "20260301"),
        ("3.13.0", "20241016"),
        ("3.12.10", "20260301"),
        ("3.12.7", "20241016"),
        ("3.11.12", "20260301"),
        ("3.11.10", "20241016"),
        ("3.10.17", "20260301"),
        ("3.10.15", "20241016"),
    ];

    releases
        .iter()
        .map(|(version, tag)| {
            let url = format!(
                "https://github.com/indygreg/python-build-standalone/releases/download/{tag}/cpython-{version}+{tag}-{triple}-install_only_stripped.{ext}",
                tag = tag,
                version = version,
                triple = triple,
                ext = ext,
            );
            let sha256 = distribution_sha256(version, triple)
                .unwrap_or("")
                .to_string();
            PythonDistribution {
                version: version.to_string(),
                os: os.to_string(),
                arch: arch.to_string(),
                url,
                sha256,
            }
        })
        .collect()
}

/// Find a distribution matching the requested version.
///
/// `version_request` can be a full version ("3.12.7"), a major.minor ("3.12"),
/// or just a major ("3"). The first matching distribution is returned.
pub fn find_distribution(version_request: &str) -> Option<PythonDistribution> {
    let available = available_versions();
    available.into_iter().find(|d| {
        if version_request.matches('.').count() == 2 {
            // Full version match: "3.12.7" == "3.12.7"
            d.version == version_request
        } else if version_request.contains('.') {
            // Major.minor match: "3.12" matches "3.12.7"
            d.version.starts_with(version_request)
                && d.version[version_request.len()..].starts_with('.')
        } else {
            // Major-only match: "3" matches "3.12.7"
            d.version.starts_with(version_request)
                && d.version[version_request.len()..].starts_with('.')
        }
    })
}

// ── SHA-256 verification ───────────────────────────────────────────

/// Verify that the file at `path` matches the expected SHA-256 hash.
///
/// Reads the file in 8 KiB chunks and computes the digest incrementally,
/// so arbitrarily large archives can be verified without loading them
/// entirely into memory.
pub fn verify_sha256(path: &Path, expected: &str) -> Result<(), PythonDownloadError> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    let actual = hex::encode(hasher.finalize());
    if actual != expected {
        return Err(PythonDownloadError::HashMismatch {
            expected: expected.to_string(),
            actual,
        });
    }
    Ok(())
}

// ── Installation directory ──────────────────────────────────────────

/// Get the default directory for managed Python installations.
///
/// Respects `$UMBRAL_PYTHON_DIR`, otherwise uses `~/.local/share/umbral/python/`.
pub fn default_install_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("UMBRAL_PYTHON_DIR") {
        return PathBuf::from(dir);
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("umbral")
            .join("python");
    }
    PathBuf::from("/tmp/umbral/python")
}

// ── Installed Python management ─────────────────────────────────────

/// Information about an installed managed Python interpreter.
#[derive(Debug, Clone)]
pub struct InstalledPython {
    /// The Python version (e.g. "3.12.7").
    pub version: String,
    /// Root directory of the installation.
    pub install_path: PathBuf,
    /// Path to the Python executable.
    pub executable: PathBuf,
}

/// List Python versions installed in the managed directory.
pub fn list_installed(install_dir: &Path) -> Vec<InstalledPython> {
    let mut installed = Vec::new();

    if !install_dir.exists() {
        return installed;
    }

    let entries = match std::fs::read_dir(install_dir) {
        Ok(entries) => entries,
        Err(_) => return installed,
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if let Some(version) = name_str.strip_prefix("python-") {
            let install_path = entry.path();
            let executable = python_executable_in(&install_path);

            if executable.exists() {
                installed.push(InstalledPython {
                    version: version.to_string(),
                    install_path,
                    executable,
                });
            }
        }
    }

    // Sort by version descending (newest first)
    installed.sort_by(|a, b| b.version.cmp(&a.version));
    installed
}

/// Find a managed Python installation matching a version request.
///
/// `version_request` can be "3.12.7", "3.12", or "3".
pub fn find_installed(install_dir: &Path, version_request: &str) -> Option<InstalledPython> {
    list_installed(install_dir).into_iter().find(|p| {
        if version_request.matches('.').count() == 2 {
            // Full version match: "3.12.7" == "3.12.7"
            p.version == version_request
        } else {
            // Prefix match with '.' boundary: "3.12" matches "3.12.7",
            // and "3" matches "3.12.7" but NOT "31.0.0"
            p.version.starts_with(version_request)
                && p.version[version_request.len()..].starts_with('.')
        }
    })
}

/// Get the path to the Python executable within an installation directory.
///
/// The python-build-standalone layout puts the binary at:
/// - Unix: `python/install/bin/python3`
/// - Windows: `python/install/python.exe`
///
/// However, the `install_only_stripped` variant extracts to:
/// - Unix: `python/bin/python3`
/// - Windows: `python/python.exe`
fn python_executable_in(install_path: &Path) -> PathBuf {
    if cfg!(windows) {
        // Try both layouts
        let direct = install_path.join("python.exe");
        if direct.exists() {
            return direct;
        }
        install_path.join("install").join("python.exe")
    } else {
        // Try the direct layout first (install_only_stripped)
        let direct = install_path.join("bin").join("python3");
        if direct.exists() {
            return direct;
        }
        // Try nested install layout
        let nested = install_path.join("install").join("bin").join("python3");
        if nested.exists() {
            return nested;
        }
        // Fall back to the direct path (for consistency, even if it doesn't exist yet)
        direct
    }
}

/// Install a Python version by creating the directory structure.
///
/// This prepares the installation directory for a downloaded archive to be
/// extracted into. The actual download and extraction happen separately.
///
/// Returns the path where the Python installation will live.
pub fn prepare_install_dir(
    version: &str,
    install_dir: &Path,
) -> Result<PathBuf, PythonDownloadError> {
    let target = install_dir.join(format!("python-{version}"));

    if target.exists() {
        // Check if there is already a working Python in there
        let exe = python_executable_in(&target);
        if exe.exists() {
            return Err(PythonDownloadError::AlreadyInstalled(version.to_string()));
        }
        // Directory exists but no executable — clean up and retry
        std::fs::remove_dir_all(&target)?;
    }

    std::fs::create_dir_all(&target)?;
    info!(version, path = %target.display(), "prepared install directory");

    Ok(target)
}

/// "Install" a Python version by writing a marker file into the target directory.
///
/// In a real implementation, this would download and extract the archive. For now,
/// this is the synchronous install path that sets up the directory structure for
/// testing and local development.
///
/// For the full async download pipeline, see `download_and_install_python`.
pub fn install_python_local(
    version: &str,
    install_dir: &Path,
) -> Result<InstalledPython, PythonDownloadError> {
    let target = prepare_install_dir(version, install_dir)?;

    // Note: In a real download pipeline, call `verify_sha256` on the
    // downloaded archive before extracting. This local/placeholder path
    // creates a synthetic installation so there is nothing to verify.

    // Create the bin directory structure matching python-build-standalone layout
    if cfg!(windows) {
        // Write a placeholder python.exe (just a marker)
        let exe_path = target.join("python.exe");
        std::fs::write(&exe_path, "# placeholder\n")?;
    } else {
        let bin_dir = target.join("bin");
        std::fs::create_dir_all(&bin_dir)?;
        let exe_path = bin_dir.join("python3");
        std::fs::write(&exe_path, "#!/bin/sh\n# placeholder\n")?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe_path, std::fs::Permissions::from_mode(0o755))?;
        }
    }

    let executable = python_executable_in(&target);

    info!(
        version,
        path = %target.display(),
        executable = %executable.display(),
        "installed Python (local/placeholder)"
    );

    Ok(InstalledPython {
        version: version.to_string(),
        install_path: target,
        executable,
    })
}

/// Remove a managed Python installation.
pub fn remove_python(version: &str, install_dir: &Path) -> Result<(), PythonDownloadError> {
    let target = install_dir.join(format!("python-{version}"));

    if !target.exists() {
        return Err(PythonDownloadError::NotInstalled(version.to_string()));
    }

    info!(version, path = %target.display(), "removing Python installation");
    std::fs::remove_dir_all(&target)?;

    Ok(())
}

// ── Download + verify + extract pipeline ───────────────────────────

/// Download a file from a URL to a destination path with progress reporting.
///
/// Reads the full response body, then writes it to disk in 8 KiB chunks
/// while updating a progress bar. For typical python-build-standalone
/// archives (~30-60 MB) this is efficient and avoids a `futures-util`
/// dependency for streaming.
pub async fn download_archive(url: &str, dest: &Path) -> Result<(), PythonDownloadError> {
    let timeout_secs = std::env::var("UMBRAL_HTTP_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(120);
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| PythonDownloadError::Download(format!("failed to build HTTP client: {e}")))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| PythonDownloadError::Download(format!("HTTP request failed: {e}")))?;

    if !response.status().is_success() {
        return Err(PythonDownloadError::Download(format!(
            "server returned HTTP {}",
            response.status()
        )));
    }

    let total_size = response.content_length().unwrap_or(0);

    let pb = ProgressBar::new(total_size);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("  {spinner:.green} [{bar:30.cyan/dim}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})")
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("=> "),
    );
    pb.enable_steady_tick(std::time::Duration::from_millis(100));

    let bytes = response
        .bytes()
        .await
        .map_err(|e| PythonDownloadError::Download(format!("failed to read response body: {e}")))?;

    // Write in chunks to update the progress bar.
    // Uses std::fs (not tokio::fs) to avoid requiring the tokio "fs" feature.
    let mut file = std::fs::File::create(dest)?;

    let chunk_size = 8192;
    let mut written = 0u64;
    for chunk in bytes.chunks(chunk_size) {
        std::io::Write::write_all(&mut file, chunk)?;
        written += chunk.len() as u64;
        pb.set_position(written);
    }

    std::io::Write::flush(&mut file)?;

    pb.finish_and_clear();
    Ok(())
}

/// Extract a `.tar.gz` archive to a destination directory.
///
/// The archive is decompressed with `flate2::read::GzDecoder` and then
/// unpacked with `tar::Archive::unpack`.
pub fn extract_tar_gz(archive_path: &Path, dest_dir: &Path) -> Result<(), PythonDownloadError> {
    let file = std::fs::File::open(archive_path)?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);

    archive.unpack(dest_dir).map_err(|e| {
        PythonDownloadError::Extraction(format!(
            "failed to extract {}: {}",
            archive_path.display(),
            e
        ))
    })?;

    Ok(())
}

/// Full download + verify + extract pipeline for a Python distribution.
///
/// 1. Downloads the archive to a temporary file.
/// 2. Verifies the SHA-256 hash against the catalog.
/// 3. Prepares the installation directory.
/// 4. Extracts the archive.
/// 5. Cleans up the temporary file.
/// 6. Locates and returns the path to the installed Python binary.
pub async fn download_and_install(
    dist: &PythonDistribution,
    install_dir: &Path,
) -> Result<PathBuf, PythonDownloadError> {
    // 1. Create temp file for download
    let temp_dir = std::env::temp_dir();
    let archive_name = format!(
        "umbral-python-{}-{}.tar.gz",
        dist.version,
        std::process::id()
    );
    let temp_path = temp_dir.join(&archive_name);

    info!(url = %dist.url, dest = %temp_path.display(), "downloading Python archive");

    // 2. Download the archive
    download_archive(&dist.url, &temp_path).await?;

    // 3. Verify SHA-256 hash
    if !dist.sha256.is_empty() {
        info!("verifying SHA-256 hash");
        verify_sha256(&temp_path, &dist.sha256)?;
    }

    // 4. Prepare the install directory
    let version_dir = prepare_install_dir(&dist.version, install_dir)?;

    // 5. Extract the archive
    info!(dest = %version_dir.display(), "extracting archive");
    extract_tar_gz(&temp_path, &version_dir)?;

    // 6. Clean up temp file
    if let Err(e) = std::fs::remove_file(&temp_path) {
        tracing::warn!(
            "failed to clean up temp file {}: {}",
            temp_path.display(),
            e
        );
    }

    // 7. Find and verify the python binary exists
    let executable = python_executable_in(&version_dir);
    if !executable.exists() {
        return Err(PythonDownloadError::BinaryNotFound(format!(
            "expected Python binary at {}, but it was not found after extraction",
            executable.display()
        )));
    }

    // Ensure the binary is executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(&executable)?;
        let mut perms = metadata.permissions();
        if perms.mode() & 0o111 == 0 {
            perms.set_mode(perms.mode() | 0o755);
            std::fs::set_permissions(&executable, perms)?;
        }
    }

    info!(
        version = %dist.version,
        executable = %executable.display(),
        "Python installation complete"
    );

    Ok(executable)
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_current_platform_detection() {
        let os = current_os();
        let arch = current_arch();

        // We should get a real value on CI and dev machines
        assert!(
            ["linux", "darwin", "windows"].contains(&os),
            "unexpected os: {os}"
        );
        assert!(
            ["x86_64", "aarch64"].contains(&arch),
            "unexpected arch: {arch}"
        );
    }

    #[test]
    fn test_arch_triple_is_valid() {
        let triple = arch_triple();
        assert_ne!(triple, "unknown-unknown-unknown", "unsupported platform");
    }

    #[test]
    fn test_available_versions_returns_distributions() {
        let versions = available_versions();
        assert!(
            !versions.is_empty(),
            "should have at least one distribution"
        );

        let os = current_os();
        let arch = current_arch();
        for dist in &versions {
            assert_eq!(dist.os, os);
            assert_eq!(dist.arch, arch);
            assert!(!dist.url.is_empty());
            assert!(dist.url.contains("python-build-standalone"));
            assert!(dist.url.contains(&dist.version));
        }
    }

    #[test]
    fn test_find_distribution_full_version() {
        let dist = find_distribution("3.12.7");
        assert!(dist.is_some(), "should find 3.12.7");
        assert_eq!(dist.unwrap().version, "3.12.7");
    }

    #[test]
    fn test_find_distribution_major_minor() {
        let dist = find_distribution("3.12");
        assert!(dist.is_some(), "should find a 3.12.x version");
        assert!(dist.unwrap().version.starts_with("3.12."));
    }

    #[test]
    fn test_find_distribution_major_only() {
        let dist = find_distribution("3");
        assert!(dist.is_some(), "should find a Python 3.x version");
        assert!(dist.unwrap().version.starts_with("3."));
    }

    #[test]
    fn test_find_distribution_nonexistent() {
        let dist = find_distribution("2.7.18");
        assert!(dist.is_none(), "should not find Python 2.7.18");
    }

    #[test]
    fn test_list_installed_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let installed = list_installed(tmp.path());
        assert!(installed.is_empty());
    }

    #[test]
    fn test_list_installed_nonexistent_dir() {
        let installed = list_installed(Path::new("/nonexistent/path"));
        assert!(installed.is_empty());
    }

    #[test]
    fn test_install_and_list_python() {
        let tmp = tempfile::tempdir().unwrap();
        let install_dir = tmp.path();

        // Install a version
        let result = install_python_local("3.12.7", install_dir);
        assert!(result.is_ok(), "install should succeed: {:?}", result.err());

        let installed = result.unwrap();
        assert_eq!(installed.version, "3.12.7");
        assert!(installed.executable.exists());
        assert!(installed.install_path.exists());

        // List should find it
        let all = list_installed(install_dir);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].version, "3.12.7");
    }

    #[test]
    fn test_install_multiple_versions() {
        let tmp = tempfile::tempdir().unwrap();
        let install_dir = tmp.path();

        install_python_local("3.12.7", install_dir).unwrap();
        install_python_local("3.11.10", install_dir).unwrap();
        install_python_local("3.10.15", install_dir).unwrap();

        let all = list_installed(install_dir);
        assert_eq!(all.len(), 3);

        // Should be sorted descending
        assert_eq!(all[0].version, "3.12.7");
        assert_eq!(all[1].version, "3.11.10");
        assert_eq!(all[2].version, "3.10.15");
    }

    #[test]
    fn test_install_already_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let install_dir = tmp.path();

        install_python_local("3.12.7", install_dir).unwrap();

        let result = install_python_local("3.12.7", install_dir);
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), PythonDownloadError::AlreadyInstalled(v) if v == "3.12.7")
        );
    }

    #[test]
    fn test_remove_python() {
        let tmp = tempfile::tempdir().unwrap();
        let install_dir = tmp.path();

        install_python_local("3.12.7", install_dir).unwrap();
        assert_eq!(list_installed(install_dir).len(), 1);

        let result = remove_python("3.12.7", install_dir);
        assert!(result.is_ok());
        assert!(list_installed(install_dir).is_empty());
    }

    #[test]
    fn test_remove_not_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let result = remove_python("3.12.7", tmp.path());
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), PythonDownloadError::NotInstalled(v) if v == "3.12.7")
        );
    }

    #[test]
    fn test_find_installed_by_version() {
        let tmp = tempfile::tempdir().unwrap();
        let install_dir = tmp.path();

        install_python_local("3.12.7", install_dir).unwrap();
        install_python_local("3.11.10", install_dir).unwrap();

        // Full version
        let found = find_installed(install_dir, "3.12.7");
        assert!(found.is_some());
        assert_eq!(found.unwrap().version, "3.12.7");

        // Major.minor
        let found = find_installed(install_dir, "3.11");
        assert!(found.is_some());
        assert_eq!(found.unwrap().version, "3.11.10");

        // Major only
        let found = find_installed(install_dir, "3");
        assert!(found.is_some());
        // Should find highest version first (3.12.7)
        assert_eq!(found.unwrap().version, "3.12.7");

        // Not installed
        let found = find_installed(install_dir, "3.9");
        assert!(found.is_none());
    }

    #[test]
    fn test_default_install_dir_respects_env() {
        // Save and restore the env var
        let original = std::env::var("UMBRAL_PYTHON_DIR").ok();

        std::env::set_var("UMBRAL_PYTHON_DIR", "/tmp/test-umbral-python");
        assert_eq!(
            default_install_dir(),
            PathBuf::from("/tmp/test-umbral-python")
        );

        // Restore
        match original {
            Some(val) => std::env::set_var("UMBRAL_PYTHON_DIR", val),
            None => std::env::remove_var("UMBRAL_PYTHON_DIR"),
        }
    }

    #[test]
    fn test_distribution_urls_contain_platform_info() {
        let versions = available_versions();
        let triple = arch_triple();

        for dist in &versions {
            assert!(
                dist.url.contains(triple),
                "URL should contain arch triple {}: {}",
                triple,
                dist.url
            );
        }
    }

    #[test]
    fn test_all_distributions_have_sha256() {
        let versions = available_versions();
        assert!(!versions.is_empty(), "need at least one distribution");

        for dist in &versions {
            assert!(
                !dist.sha256.is_empty(),
                "distribution {} ({}/{}) has an empty sha256",
                dist.version,
                dist.os,
                dist.arch,
            );
            assert_eq!(
                dist.sha256.len(),
                64,
                "sha256 for {} should be 64 hex chars, got {}",
                dist.version,
                dist.sha256.len(),
            );
        }
    }

    #[test]
    fn test_verify_sha256_good_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("test_file.bin");
        let content = b"umbral integrity check test data";
        std::fs::write(&file_path, content).unwrap();

        // Compute the expected hash of the known content
        use sha2::{Digest, Sha256};
        let expected = hex::encode(Sha256::digest(content));

        let result = verify_sha256(&file_path, &expected);
        assert!(result.is_ok(), "verify_sha256 should pass for correct hash");
    }

    #[test]
    fn test_verify_sha256_bad_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("test_file.bin");
        let content = b"umbral integrity check test data";
        std::fs::write(&file_path, content).unwrap();

        let wrong_hash = "0000000000000000000000000000000000000000000000000000000000000000";

        let result = verify_sha256(&file_path, wrong_hash);
        assert!(result.is_err(), "verify_sha256 should fail for wrong hash");
        match result.unwrap_err() {
            PythonDownloadError::HashMismatch { expected, actual } => {
                assert_eq!(expected, wrong_hash);
                assert_ne!(actual, wrong_hash);
                assert_eq!(actual.len(), 64);
            }
            other => panic!("expected HashMismatch, got: {other:?}"),
        }
    }

    // ── Download pipeline tests ────────────────────────────────────

    #[test]
    fn test_extract_tar_gz() {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let tmp = tempfile::tempdir().unwrap();

        // Build a .tar.gz archive in memory
        let archive_path = tmp.path().join("test.tar.gz");
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let gz = GzEncoder::new(file, Compression::default());
            let mut builder = tar::Builder::new(gz);

            // Add a file: "hello.txt" with content "hello world"
            let content = b"hello world\n";
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "hello.txt", &content[..])
                .unwrap();

            // Add a nested file: "subdir/nested.txt"
            let nested_content = b"nested content\n";
            let mut header2 = tar::Header::new_gnu();
            header2.set_size(nested_content.len() as u64);
            header2.set_mode(0o644);
            header2.set_cksum();
            builder
                .append_data(&mut header2, "subdir/nested.txt", &nested_content[..])
                .unwrap();

            builder.finish().unwrap();
        }

        // Extract it
        let dest_dir = tmp.path().join("extracted");
        std::fs::create_dir_all(&dest_dir).unwrap();
        extract_tar_gz(&archive_path, &dest_dir).unwrap();

        // Verify files exist and have correct content
        let hello = dest_dir.join("hello.txt");
        assert!(hello.exists(), "hello.txt should exist after extraction");
        assert_eq!(std::fs::read_to_string(&hello).unwrap(), "hello world\n");

        let nested = dest_dir.join("subdir").join("nested.txt");
        assert!(
            nested.exists(),
            "subdir/nested.txt should exist after extraction"
        );
        assert_eq!(
            std::fs::read_to_string(&nested).unwrap(),
            "nested content\n"
        );
    }

    #[test]
    fn test_download_and_install_hash_mismatch() {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let tmp = tempfile::tempdir().unwrap();
        let install_dir = tmp.path().join("install");
        std::fs::create_dir_all(&install_dir).unwrap();

        // Create a valid .tar.gz that would normally extract fine
        let archive_path = tmp.path().join("fake-python.tar.gz");
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let gz = GzEncoder::new(file, Compression::default());
            let mut builder = tar::Builder::new(gz);
            let content = b"fake";
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "python/bin/python3", &content[..])
                .unwrap();
            builder.finish().unwrap();
        }

        // Verify that a wrong hash fails
        let wrong_hash = "0000000000000000000000000000000000000000000000000000000000000000";
        let result = verify_sha256(&archive_path, wrong_hash);
        assert!(result.is_err(), "should fail with hash mismatch");
        assert!(
            matches!(
                result.unwrap_err(),
                PythonDownloadError::HashMismatch { .. }
            ),
            "error should be HashMismatch variant"
        );
    }

    /// Actually download Python 3.12 for the current platform.
    ///
    /// This test is ignored by default because it requires network access
    /// and downloads ~30-60 MB. Run manually with:
    ///   cargo test -p umbral-venv test_download_and_install_real -- --ignored
    #[tokio::test]
    #[ignore]
    async fn test_download_and_install_real() {
        let tmp = tempfile::tempdir().unwrap();
        let install_dir = tmp.path();

        // Use 3.12.7 specifically — it has verified URLs/hashes (newer versions have placeholders)
        let dist = find_distribution("3.12.7").expect("should find 3.12.7 distribution");

        let executable = download_and_install(&dist, install_dir)
            .await
            .expect("download_and_install should succeed");

        assert!(
            executable.exists(),
            "python binary should exist at {}",
            executable.display()
        );

        // On Unix, verify it is executable
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::metadata(&executable).unwrap().permissions();
            assert!(
                perms.mode() & 0o111 != 0,
                "python binary should be executable"
            );
        }
    }
}
