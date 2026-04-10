pub mod add;
pub mod build;
pub mod init;
pub mod install;
pub mod pip;
pub mod publish;
pub mod python;
pub mod remove;
pub mod resolve;
pub mod run;
pub mod sync;
pub mod tool;
pub mod venv;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use miette::{Context, IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use tracing::warn;
use umbral_installer::InstalledPackage;
use umbral_pep440::PackageName;
use umbral_project::workspace::Workspace;
use umbral_pypi_client::{PlatformTags, WheelFilename};

/// Maximum number of concurrent wheel downloads.
const MAX_CONCURRENT_DOWNLOADS: usize = 8;

/// Detect the Python version that will actually be used for the venv.
/// Returns a major.minor version string like "3.14".
///
/// Uses the same discovery logic as venv creation (`PythonInterpreter::find`)
/// to ensure the resolver targets the same Python that will be installed into.
pub fn detect_python_version() -> String {
    // 1. Try reading from existing .venv/pyvenv.cfg
    if let Ok(cfg) = std::fs::read_to_string(".venv/pyvenv.cfg") {
        for line in cfg.lines() {
            if let Some(rest) = line.strip_prefix("version") {
                let rest = rest.trim_start_matches([' ', '=']);
                let parts: Vec<&str> = rest.trim().split('.').collect();
                if parts.len() >= 2 {
                    return format!("{}.{}", parts[0], parts[1]);
                }
            }
        }
    }

    // 2. Use PythonInterpreter::find — same logic as venv creation
    if let Ok(interp) = umbral_venv::PythonInterpreter::find(None) {
        return interp.major_minor;
    }

    // 3. Fallback
    "3.12".to_string()
}

/// Return the platform-appropriate cache directory for umbral.
pub fn dirs_cache_dir() -> PathBuf {
    // XDG_CACHE_HOME or platform default
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        return PathBuf::from(xdg).join("umbral");
    }
    if cfg!(target_os = "macos") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Caches")
                .join("umbral");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".cache").join("umbral");
    }
    PathBuf::from(".umbral-cache")
}

/// Normalize a Python package name per PEP 503.
///
/// Delegates to [`PackageName`] which is the single source of truth for
/// PEP 503 normalization across the workspace.
pub fn normalize_package_name(name: &str) -> String {
    PackageName::new(name).as_str().to_string()
}

/// Download and install a set of packages into a virtual environment.
///
/// This is the shared pipeline used by both `install` and `sync` commands.
/// It downloads wheels concurrently (up to `MAX_CONCURRENT_DOWNLOADS` at a time)
/// and then installs them sequentially.
pub fn download_and_install_packages(
    packages: &[(&str, &str)], // (name, version) pairs
    index_url: &str,
    site_packages: &Path,
    python_path: &Path,
    cache_dir: &Path,
    progress_verb: &str,
    venv_root: Option<&Path>,
) -> Result<()> {
    if packages.is_empty() {
        return Ok(());
    }

    let bin_dir = python_path
        .parent()
        .ok_or_else(|| miette::miette!("python path has no parent directory"))?;

    let wheels_cache_dir = cache_dir.join("wheels");
    let installer = umbral_installer::WheelInstaller::with_cache_dir(
        wheels_cache_dir.clone(),
        umbral_installer::LinkMode::default(),
    );

    let rt = tokio::runtime::Runtime::new()
        .into_diagnostic()
        .wrap_err("failed to create async runtime")?;

    let parsed_index_url: url::Url = index_url
        .parse()
        .into_diagnostic()
        .wrap_err("invalid index URL")?;

    let pypi_cache_dir = cache_dir.join("pypi");
    let client = Arc::new(
        umbral_pypi_client::SimpleApiClient::new(parsed_index_url, pypi_cache_dir)
            .into_diagnostic()
            .wrap_err("failed to create PyPI client")?,
    );

    let multi_progress = indicatif::MultiProgress::new();
    let overall_pb = multi_progress.add(indicatif::ProgressBar::new(packages.len() as u64));
    overall_pb.set_style(
        indicatif::ProgressStyle::with_template(&format!(
            "  {{spinner:.green}} [{{pos}}/{{len}}] {} packages...",
            progress_verb,
        ))
        .expect("progress template is valid")
        .tick_chars(
            "\u{280b}\u{2819}\u{2839}\u{2838}\u{283c}\u{2834}\u{2826}\u{2827}\u{2807}\u{280f} ",
        ),
    );
    overall_pb.enable_steady_tick(std::time::Duration::from_millis(80));

    // Detect platform tags for the target Python interpreter.
    let platform_tags = PlatformTags::detect(python_path)
        .into_diagnostic()
        .wrap_err("failed to detect platform tags from Python interpreter")?;
    let platform_tags = Arc::new(platform_tags);

    // Collect owned copies for the async block.
    let owned_packages: Vec<(String, String)> = packages
        .iter()
        .map(|(n, v)| (n.to_string(), v.to_string()))
        .collect();

    let owned_wheels_cache_dir = wheels_cache_dir.clone();

    // Download all wheels concurrently, then install sequentially.
    let download_results: Vec<std::result::Result<(String, String, PathBuf), miette::Report>> =
        rt.block_on(async {
            use tokio::sync::Semaphore;

            let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_DOWNLOADS));
            let mut handles = Vec::new();

            for (pkg_name, pkg_version) in &owned_packages {
                let client = Arc::clone(&client);
                let sem = Arc::clone(&semaphore);
                let tags = Arc::clone(&platform_tags);
                let pkg_name = pkg_name.clone();
                let pkg_version = pkg_version.clone();
                let mp = multi_progress.clone();
                let task_wheels_cache_dir = owned_wheels_cache_dir.clone();

                handles.push(tokio::spawn(async move {
                    let _permit = sem.acquire().await.map_err(|e| {
                        miette::miette!("semaphore error: {}", e)
                    })?;

                    let dl_pb = mp.add(indicatif::ProgressBar::new_spinner());
                    dl_pb.set_style(
                        indicatif::ProgressStyle::with_template("    {spinner:.cyan} {msg}")
                            .expect("progress template is valid")
                            .tick_chars("\u{280b}\u{2819}\u{2839}\u{2838}\u{283c}\u{2834}\u{2826}\u{2827}\u{2807}\u{280f} "),
                    );
                    dl_pb.set_message(format!("downloading {} {}", pkg_name, pkg_version));
                    dl_pb.enable_steady_tick(std::time::Duration::from_millis(80));

                    // Fetch the project page to find the wheel URL
                    let page = client
                        .fetch_project_page(&pkg_name)
                        .await
                        .map_err(|e| {
                            miette::miette!(
                                "failed to fetch project page for {}: {}",
                                pkg_name,
                                e
                            )
                        })?;

                    // Find the best compatible wheel for this exact version
                    // using platform tag detection. Considers native wheels,
                    // abi3 wheels, and pure-Python wheels, preferring the most
                    // specific match (lowest compatibility score).
                    let wheel_file = page
                        .files
                        .iter()
                        .filter_map(|f| {
                            let wf = WheelFilename::parse(&f.filename).ok()?;
                            if wf.version != pkg_version {
                                return None;
                            }
                            let score = tags.compatibility_score(&f.filename)?;
                            Some((f, score))
                        })
                        .min_by_key(|(_, score)| *score)
                        .map(|(f, _)| f);

                    let dl_installer = umbral_installer::WheelInstaller::with_cache_dir(
                        task_wheels_cache_dir,
                        umbral_installer::LinkMode::default(),
                    );

                    let wheel_path = if let Some(wheel_file) = wheel_file {
                        // Happy path: compatible wheel found, download it
                        let sha256 = wheel_file.hashes.get("sha256").map(|s| s.as_str());

                        dl_installer
                            .download_wheel(&wheel_file.url, &wheel_file.filename, sha256)
                            .await
                            .map_err(|e| {
                                miette::miette!(
                                    "failed to download wheel for {}: {}",
                                    pkg_name,
                                    e
                                )
                            })?
                    } else {
                        // Fallback: no compatible wheel — try building from sdist
                        dl_pb.set_message(format!("building {} {} from source", pkg_name, pkg_version));

                        // Find an sdist (.tar.gz) for this version
                        let sdist_file = page
                            .files
                            .iter()
                            .find(|f| {
                                f.filename.ends_with(".tar.gz")
                                    && f.filename.contains(&pkg_version)
                            })
                            .ok_or_else(|| {
                                miette::miette!(
                                    "No compatible wheel or sdist found for {} {} on this platform.\n\
                                     hint: check your `requires-python` constraint and ensure the package \
                                     supports your platform and Python version",
                                    pkg_name,
                                    pkg_version
                                )
                            })?;

                        let sdist_sha256 = sdist_file.hashes.get("sha256").map(|s| s.as_str());

                        // Download the sdist
                        let sdist_path = dl_installer
                            .download_wheel(&sdist_file.url, &sdist_file.filename, sdist_sha256)
                            .await
                            .map_err(|e| {
                                miette::miette!(
                                    "failed to download sdist for {}: {}",
                                    pkg_name,
                                    e
                                )
                            })?;

                        // Extract and build in a blocking context since build
                        // involves subprocess spawning
                        let sdist_path_owned = sdist_path.clone();
                        let pkg_name_owned = pkg_name.clone();
                        tokio::task::spawn_blocking(move || {
                            let build_tmp = tempfile::tempdir().map_err(|e| {
                                miette::miette!("failed to create temp dir for build: {}", e)
                            })?;

                            // Extract the sdist
                            let source_dir = umbral_installer::build::extract_sdist(
                                &sdist_path_owned,
                                build_tmp.path(),
                            )
                            .map_err(|e| {
                                miette::miette!(
                                    "failed to extract sdist for {}: {}",
                                    pkg_name_owned,
                                    e
                                )
                            })?;

                            // Read pyproject.toml from the extracted source to get
                            // build-system configuration
                            let pyproject_path = source_dir.join("pyproject.toml");
                            let (build_backend, requires, backend_path) =
                                if pyproject_path.exists() {
                                    let pyproject =
                                        umbral_project::PyProject::from_path(&pyproject_path)
                                            .map_err(|e| {
                                                miette::miette!(
                                                    "failed to parse pyproject.toml for {}: {}",
                                                    pkg_name_owned,
                                                    e
                                                )
                                            })?;
                                    let bs = pyproject.build_system_or_default();
                                    (bs.build_backend, bs.requires, bs.backend_path)
                                } else {
                                    // Legacy: no pyproject.toml, assume setuptools
                                    (
                                        Some("setuptools.build_meta:__legacy__".to_string()),
                                        vec!["setuptools".to_string(), "wheel".to_string()],
                                        None,
                                    )
                                };

                            let backend = build_backend.ok_or_else(|| {
                                miette::miette!(
                                    "No build-backend specified for {} (no pyproject.toml or missing [build-system].build-backend). \
                                     Cannot build from sdist.",
                                    pkg_name_owned,
                                )
                            })?;

                            let config = umbral_installer::build::BuildConfig {
                                python: std::path::PathBuf::from(if cfg!(windows) { "python" } else { "python3" }),
                                build_backend: backend,
                                requires,
                                backend_path,
                            };

                            let output_dir = build_tmp.path().join("wheel-output");
                            let wheel_path = umbral_installer::build::build_wheel_from_source(
                                &source_dir,
                                &output_dir,
                                &config,
                            )
                            .map_err(|e| {
                                miette::miette!(
                                    "failed to build wheel for {} from sdist: {}",
                                    pkg_name_owned,
                                    e
                                )
                            })?;

                            // Copy the built wheel to a stable location outside the
                            // temp dir so it persists after build_tmp is dropped.
                            let wheel_filename = wheel_path
                                .file_name()
                                .ok_or_else(|| miette::miette!("built wheel has no filename"))?;
                            let stable_dir = std::env::temp_dir().join("umbral-built-wheels");
                            std::fs::create_dir_all(&stable_dir).map_err(|e| {
                                miette::miette!("failed to create built-wheels dir: {}", e)
                            })?;
                            let stable_path = stable_dir.join(wheel_filename);
                            std::fs::copy(&wheel_path, &stable_path).map_err(|e| {
                                miette::miette!("failed to copy built wheel: {}", e)
                            })?;

                            Ok::<std::path::PathBuf, miette::Report>(stable_path)
                        })
                        .await
                        .map_err(|e| miette::miette!("build task join error: {}", e))??
                    };

                    dl_pb.finish_and_clear();

                    Ok((pkg_name, pkg_version, wheel_path))
                }));
            }

            let mut results = Vec::with_capacity(handles.len());
            for handle in handles {
                let result = handle
                    .await
                    .map_err(|e| miette::miette!("task join error: {}", e))?;
                results.push(result);
            }
            Ok::<_, miette::Report>(results)
        })
        .map_err(|e: miette::Report| e)?;

    // Install sequentially (writes to same directory)
    for result in download_results {
        let (pkg_name, _pkg_version, wheel_path) = result?;

        overall_pb.set_message(format!("installing {}", pkg_name));

        installer
            .install_wheel(&wheel_path, site_packages, bin_dir, python_path, venv_root)
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to install wheel for {}", pkg_name))?;

        overall_pb.inc(1);
    }

    overall_pb.finish_and_clear();

    Ok(())
}

/// Default lockfile path.
pub const DEFAULT_LOCKFILE: &str = "./uv.lock";

/// Default virtual environment path.
pub const DEFAULT_VENV: &str = ".venv";

/// Default pyproject.toml path.
pub const DEFAULT_PROJECT: &str = "./pyproject.toml";

/// Try to discover a workspace from `start_dir`. Returns `None` if not in a workspace.
pub fn discover_workspace(start_dir: &Path) -> Option<Workspace> {
    match Workspace::discover(start_dir) {
        Ok(Some(ws)) => Some(ws),
        Ok(None) => None,
        Err(e) => {
            tracing::warn!("workspace discovery failed: {}", e);
            None
        }
    }
}

/// Ensures the project is fully synced: lockfile fresh, venv exists, packages installed.
///
/// This is the single entry point that `add`, `remove`, `sync`, `run`, and `install` all
/// funnel through so that every command does everything the user needs in one step.
pub fn ensure_synced(
    project_path: &Path,
    lockfile_path: &Path,
    venv_path: &Path,
    index_url: Option<&str>,
) -> Result<()> {
    let started = Instant::now();

    // -- Step 1: Check if lockfile exists and is fresh; resolve if not --
    let needs_resolve = if lockfile_path.exists() {
        // Use mtime-based staleness: if pyproject.toml is newer than the
        // lockfile, re-resolve. This works regardless of whether the lockfile
        // embeds an input hash (the uv.lock format does not).
        match (project_path.metadata(), lockfile_path.metadata()) {
            (Ok(proj_meta), Ok(lock_meta)) => {
                match (proj_meta.modified(), lock_meta.modified()) {
                    (Ok(proj_mtime), Ok(lock_mtime)) => proj_mtime > lock_mtime,
                    _ => true, // can't compare mtimes, re-resolve to be safe
                }
            }
            _ => true, // missing metadata, re-resolve
        }
    } else {
        true
    };

    if needs_resolve {
        eprintln!(
            "{} {} dependencies...",
            "●".green().bold(),
            "Locking".bold(),
        );
        let resolve_args = resolve::ResolveArgs::for_project(project_path.to_path_buf());
        resolve::cmd_resolve(resolve_args)?;
    }

    // -- Step 3: Create .venv if it doesn't exist --
    if !umbral_venv::is_venv(venv_path) {
        eprintln!(
            "{} Creating virtual environment at {}...",
            "●".green().bold(),
            venv_path.display().to_string().cyan(),
        );

        let interpreter = umbral_venv::PythonInterpreter::find(None)
            .into_diagnostic()
            .wrap_err("failed to find a Python interpreter to create .venv — is Python installed and on PATH?")?;

        umbral_venv::create_venv(venv_path, &interpreter, None)
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to create venv at {}", venv_path.display()))?;

        eprintln!(
            "{} Created virtual environment at {}",
            "✓".green().bold(),
            venv_path.display().to_string().cyan(),
        );
    }

    // -- Step 4: Sync packages from lockfile into the venv --
    let lockfile = umbral_lockfile::Lockfile::from_path(lockfile_path)
        .into_diagnostic()
        .wrap_err_with(|| {
            format!(
                "failed to read lockfile {} — try running `umbral lock` to regenerate it",
                lockfile_path.display()
            )
        })?;

    let lockfile_index_url = index_url
        .or(lockfile.metadata.index_url.as_deref())
        .unwrap_or("https://pypi.org/simple/");

    // For universal lockfiles, filter packages to those matching the current
    // platform. We detect the current environment and evaluate marker
    // expressions to decide which packages to install.
    let applicable_package_names: Option<std::collections::HashSet<String>> =
        if lockfile.inner.is_universal() {
            // Detect the current host platform for marker evaluation.
            let current_env = umbral_pep508::MarkerEnvironment::current();
            let names = lockfile.inner.packages_for_environment(&|marker_str| {
                // Parse and evaluate the marker against the current environment.
                match umbral_pep508::parse_markers(marker_str) {
                    Ok(tree) => tree.evaluate(&current_env),
                    Err(_) => true, // if we can't parse, include the package
                }
            });
            Some(
                names
                    .into_iter()
                    .map(|n| umbral_lockfile::normalize_pep503(&n))
                    .collect(),
            )
        } else {
            None
        };

    let site_packages = umbral_venv::venv_site_packages(venv_path).ok_or_else(|| {
        miette::miette!("could not find site-packages in {}", venv_path.display())
    })?;

    let installed = umbral_installer::scan_installed(&site_packages)
        .into_diagnostic()
        .wrap_err("failed to scan installed packages")?;

    let installed_map: HashMap<String, String> = installed
        .iter()
        .map(|p| (normalize_package_name(&p.name), p.version.clone()))
        .collect();

    let locked_map: HashMap<String, &str> = lockfile
        .packages
        .iter()
        .filter(|p| {
            // In a universal lockfile, only include packages for this platform.
            if let Some(ref applicable) = applicable_package_names {
                applicable.contains(&umbral_lockfile::normalize_pep503(&p.name))
            } else {
                true
            }
        })
        .map(|p| (normalize_package_name(&p.name), p.version.as_str()))
        .collect();

    // Diff: to install, to upgrade, to remove
    let mut to_install = Vec::new();
    let mut to_upgrade = Vec::new();
    let mut to_remove: Vec<&InstalledPackage> = Vec::new();

    for pkg in &lockfile.packages {
        // In a universal lockfile, skip packages not applicable to this platform.
        if let Some(ref applicable) = applicable_package_names {
            if !applicable.contains(&umbral_lockfile::normalize_pep503(&pkg.name)) {
                continue;
            }
        }

        let normalized = normalize_package_name(&pkg.name);
        match installed_map.get(&normalized) {
            None => to_install.push(pkg),
            Some(v) if v != &pkg.version => to_upgrade.push((pkg, v.clone())),
            Some(_) => {}
        }
    }

    for inst in &installed {
        let normalized = normalize_package_name(&inst.name);
        if !locked_map.contains_key(&normalized) {
            to_remove.push(inst);
        }
    }

    let total_changes = to_install.len() + to_upgrade.len() + to_remove.len();

    if total_changes == 0 {
        let elapsed = started.elapsed();
        eprintln!(
            "{} Synced ({} packages, {:.1?})",
            "✓".green().bold(),
            installed_map.len(),
            elapsed,
        );
        return Ok(());
    }

    eprintln!("{} {} environment...", "●".green().bold(), "Syncing".bold(),);

    if !to_install.is_empty() {
        for pkg in &to_install {
            eprintln!(
                "  {} {} {}",
                "+".green(),
                pkg.name.cyan(),
                pkg.version.green()
            );
        }
    }

    if !to_upgrade.is_empty() {
        for (pkg, old_ver) in &to_upgrade {
            eprintln!(
                "  {} {} {} -> {}",
                "~".yellow(),
                pkg.name.cyan(),
                old_ver.yellow(),
                pkg.version.green(),
            );
        }
    }

    if !to_remove.is_empty() {
        for pkg in &to_remove {
            eprintln!("  {} {} {}", "-".red(), pkg.name.cyan(), pkg.version.red());
        }
    }

    // Remove stale packages
    for pkg in &to_remove {
        if let Err(e) = remove_installed_package(pkg, &site_packages) {
            warn!(error = %e, "failed to fully remove {} {}", pkg.name, pkg.version);
        }
    }

    // Install new + upgraded packages
    let packages_to_download: Vec<(&str, &str)> = to_install
        .iter()
        .chain(to_upgrade.iter().map(|(pkg, _)| pkg))
        .map(|pkg| (pkg.name.as_str(), pkg.version.as_str()))
        .collect();

    let python_path = if cfg!(windows) {
        venv_path.join("Scripts").join("python.exe")
    } else {
        venv_path.join("bin").join("python")
    };

    download_and_install_packages(
        &packages_to_download,
        lockfile_index_url,
        &site_packages,
        &python_path,
        &dirs_cache_dir(),
        "Syncing",
        Some(venv_path),
    )?;

    let elapsed = started.elapsed();
    eprintln!(
        "{} Synced: {} installed, {} upgraded, {} removed ({:.1?})",
        "✓".green().bold(),
        to_install.len(),
        to_upgrade.len(),
        to_remove.len(),
        elapsed,
    );

    Ok(())
}

/// Remove an installed package by reading its RECORD to find all installed files,
/// removing those files, cleaning up empty directories, and finally removing the
/// `.dist-info` directory itself.
///
/// Shared by `sync` and `ensure_synced`.
pub fn remove_installed_package(pkg: &InstalledPackage, site_packages: &Path) -> Result<()> {
    let record_path = pkg.dist_info_path.join("RECORD");
    if record_path.exists() {
        let content = std::fs::read_to_string(&record_path)
            .map_err(|e| miette::miette!("Failed to read RECORD: {}", e))?;
        for line in content.lines() {
            let path = line.split(',').next().unwrap_or("").trim();
            if path.is_empty() {
                continue;
            }
            let full_path = site_packages.join(path);
            if full_path.is_file() {
                let _ = std::fs::remove_file(&full_path);
            }
        }
        // Clean up empty directories left behind
        for line in content.lines() {
            let path = line.split(',').next().unwrap_or("").trim();
            if path.is_empty() {
                continue;
            }
            let full_path = site_packages.join(path);
            if let Some(parent) = full_path.parent() {
                if parent != site_packages && parent.is_dir() {
                    let _ = std::fs::remove_dir(parent);
                }
            }
        }
    }
    if pkg.dist_info_path.exists() {
        std::fs::remove_dir_all(&pkg.dist_info_path).map_err(|e| {
            miette::miette!("Failed to remove {}: {}", pkg.dist_info_path.display(), e)
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_basic() {
        assert_eq!(normalize_package_name("Requests"), "requests");
        assert_eq!(normalize_package_name("my-package"), "my-package");
    }

    #[test]
    fn test_normalize_underscores_dots() {
        assert_eq!(normalize_package_name("my_package"), "my-package");
        assert_eq!(normalize_package_name("my.package"), "my-package");
        assert_eq!(normalize_package_name("My_Cool.Package"), "my-cool-package");
    }

    #[test]
    fn test_normalize_consecutive_separators() {
        assert_eq!(normalize_package_name("my--package"), "my-package");
        assert_eq!(normalize_package_name("my_.package"), "my-package");
    }

    #[test]
    fn test_detect_python_version_returns_valid_format() {
        let version = detect_python_version();
        let parts: Vec<&str> = version.split('.').collect();
        assert_eq!(parts.len(), 2);
        assert!(parts[0].parse::<u32>().is_ok());
        assert!(parts[1].parse::<u32>().is_ok());
    }
}
