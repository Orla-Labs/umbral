use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Args, Subcommand};
use miette::{IntoDiagnostic, Result, WrapErr};
use owo_colors::OwoColorize;
use tracing::info;

use super::{
    detect_python_version, dirs_cache_dir, download_and_install_packages, normalize_package_name,
};

#[derive(Debug, Args)]
pub struct ToolArgs {
    #[command(subcommand)]
    pub command: ToolCommand,
}

#[derive(Debug, Subcommand)]
pub enum ToolCommand {
    /// Run a tool (installs temporarily if not already installed)
    Run(ToolRunArgs),
    /// Install a tool persistently
    Install(ToolInstallArgs),
    /// List installed tools
    List,
    /// Uninstall a tool
    Uninstall(ToolUninstallArgs),
}

#[derive(Debug, Args)]
pub struct ToolRunArgs {
    /// Package name to run
    pub package: String,
    /// Arguments to pass to the tool
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
    /// Specific version to install
    #[arg(long)]
    pub version: Option<String>,
}

#[derive(Debug, Args)]
pub struct ToolInstallArgs {
    /// Package name to install
    pub package: String,
    /// Specific version
    #[arg(long)]
    pub version: Option<String>,
}

#[derive(Debug, Args)]
pub struct ToolUninstallArgs {
    /// Package name to uninstall
    pub package: String,
}

/// Return the directory where tool venvs are stored.
fn tools_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("UMBRAL_TOOLS_DIR") {
        PathBuf::from(dir)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local/share/umbral/tools")
    } else {
        PathBuf::from("/tmp/umbral/tools")
    }
}

/// Return the directory where tool symlinks/bins are placed.
fn tools_bin_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("UMBRAL_TOOLS_BIN") {
        PathBuf::from(dir)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local/bin")
    } else {
        PathBuf::from("/tmp/umbral/bin")
    }
}

/// Find console scripts installed in a tool's venv.
///
/// Returns the names of executables in the venv's bin directory,
/// excluding python, pip, and activation scripts.
fn find_tool_scripts(tool_venv: &Path) -> Vec<String> {
    let bin_dir = if cfg!(windows) {
        tool_venv.join("Scripts")
    } else {
        tool_venv.join("bin")
    };

    let mut scripts = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&bin_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("python")
                && !name.starts_with("pip")
                && !name.starts_with("activate")
                && !name.starts_with('.')
                && !name.ends_with(".cfg")
                && !name.ends_with(".fish")
                && !name.ends_with(".ps1")
                && !name.ends_with(".bat")
            {
                scripts.push(name);
            }
        }
    }
    scripts.sort();
    scripts
}

/// Resolve a single package to a version and its dependency list.
///
/// Returns `(version, [(dep_name, dep_version), ...])` including the package itself.
fn resolve_package(
    package: &str,
    version: Option<&str>,
    index_url: &str,
) -> Result<(String, Vec<(String, String)>)> {
    let requirement_str = if let Some(v) = version {
        format!("{}=={}", package, v)
    } else {
        package.to_string()
    };

    let requirement = umbral_pep508::Requirement::parse(&requirement_str)
        .map_err(|e| miette::miette!("failed to parse requirement '{}': {}", requirement_str, e))?;

    let rt = tokio::runtime::Runtime::new()
        .into_diagnostic()
        .wrap_err("failed to create async runtime")?;

    let parsed_url: url::Url = index_url
        .parse()
        .into_diagnostic()
        .wrap_err("invalid index URL")?;

    let cache_dir = dirs_cache_dir().join("pypi");

    let client = Arc::new(
        umbral_pypi_client::SimpleApiClient::new(parsed_url, cache_dir)
            .into_diagnostic()
            .wrap_err("failed to create PyPI client")?,
    );

    let source = umbral_resolver::LivePypiSource::new(Arc::clone(&client), rt.handle().clone());

    let detected_version = detect_python_version();
    let python_version: umbral_pep440::Version = detected_version
        .parse()
        .into_diagnostic()
        .wrap_err("failed to parse detected python version")?;

    let config = umbral_resolver::ResolverConfig {
        python_version,
        markers: None,
        pre_release_policy: umbral_resolver::PreReleasePolicy::Disallow,
    };

    let resolution = rt.block_on(async {
        umbral_resolver::resolve(source, config, vec![requirement])
            .map_err(|e| miette::miette!("resolution failed for {}: {}", package, e))
    })?;

    let normalized = normalize_package_name(package);

    let pkg_version = resolution
        .packages
        .get(&umbral_pep440::PackageName::new(&normalized))
        .map(|p| p.version.to_string())
        .ok_or_else(|| {
            miette::miette!("package '{}' was not found in resolution result", package)
        })?;

    let all_packages: Vec<(String, String)> = resolution
        .packages
        .iter()
        .map(|(name, pkg)| (name.as_str().to_string(), pkg.version.to_string()))
        .collect();

    Ok((pkg_version, all_packages))
}

/// Install a tool into its dedicated venv and create bin symlinks.
///
/// Returns the version that was installed and the list of scripts.
fn install_tool(package: &str, version: Option<&str>) -> Result<(String, Vec<String>)> {
    let tdir = tools_dir();
    let bdir = tools_bin_dir();
    let normalized = normalize_package_name(package);
    let tool_venv = tdir.join(&normalized);

    let index_url = "https://pypi.org/simple/";

    // Resolve
    eprintln!(
        "{} {} {}...",
        "●".green().bold(),
        "Resolving".bold(),
        package.cyan(),
    );

    let (pkg_version, all_packages) = resolve_package(package, version, index_url)?;

    eprintln!(
        "  {} resolved {} {}",
        "→".dimmed(),
        normalized.cyan(),
        pkg_version.green(),
    );

    // Create tool venv
    let interpreter = umbral_venv::PythonInterpreter::find(None)
        .into_diagnostic()
        .wrap_err("failed to find a Python interpreter")?;

    // Remove old venv if it exists
    if tool_venv.exists() {
        std::fs::remove_dir_all(&tool_venv)
            .into_diagnostic()
            .wrap_err_with(|| {
                format!("failed to remove old tool venv at {}", tool_venv.display())
            })?;
    }

    umbral_venv::create_venv(&tool_venv, &interpreter, Some(&normalized))
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to create tool venv at {}", tool_venv.display()))?;

    // Install packages
    let site_packages = umbral_venv::venv_site_packages(&tool_venv).ok_or_else(|| {
        miette::miette!("could not find site-packages in {}", tool_venv.display())
    })?;

    let python_path = if cfg!(windows) {
        tool_venv.join("Scripts").join("python.exe")
    } else {
        tool_venv.join("bin").join("python")
    };

    let packages_to_install: Vec<(&str, &str)> = all_packages
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();

    download_and_install_packages(
        &packages_to_install,
        index_url,
        &site_packages,
        &python_path,
        &dirs_cache_dir(),
        "Installing",
        Some(&tool_venv),
    )?;

    // Create bin directory and symlinks
    std::fs::create_dir_all(&bdir)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to create bin directory {}", bdir.display()))?;

    let scripts = find_tool_scripts(&tool_venv);
    let tool_bin_dir = if cfg!(windows) {
        tool_venv.join("Scripts")
    } else {
        tool_venv.join("bin")
    };

    for script in &scripts {
        let src = tool_bin_dir.join(script);
        let dst = bdir.join(script);

        // Remove existing symlink/file if present
        if dst.exists() || dst.read_link().is_ok() {
            let _ = std::fs::remove_file(&dst);
        }

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&src, &dst)
                .into_diagnostic()
                .wrap_err_with(|| {
                    format!("failed to symlink {} -> {}", dst.display(), src.display())
                })?;
        }

        #[cfg(windows)]
        {
            // On Windows, create a .cmd wrapper that delegates to the tool's script
            let cmd_dst = dst.with_extension("cmd");
            let content = format!("@echo off\n\"{}\" %*\n", src.display());
            std::fs::write(&cmd_dst, content)
                .into_diagnostic()
                .wrap_err_with(|| format!("failed to write cmd wrapper {}", cmd_dst.display()))?;
        }
    }

    Ok((pkg_version, scripts))
}

pub fn cmd_tool(args: ToolArgs) -> Result<()> {
    match args.command {
        ToolCommand::Run(run_args) => cmd_tool_run(run_args),
        ToolCommand::Install(install_args) => cmd_tool_install(install_args),
        ToolCommand::List => cmd_tool_list(),
        ToolCommand::Uninstall(uninstall_args) => cmd_tool_uninstall(uninstall_args),
    }
}

fn cmd_tool_run(args: ToolRunArgs) -> Result<()> {
    let tdir = tools_dir();
    let normalized = normalize_package_name(&args.package);
    let tool_venv = tdir.join(&normalized);

    // Check if tool is already installed
    let tool_bin_dir = if cfg!(windows) {
        tool_venv.join("Scripts")
    } else {
        tool_venv.join("bin")
    };

    let script_path = tool_bin_dir.join(&normalized);

    if !script_path.exists() {
        // Not installed yet; install to a cached location
        info!("tool '{}' not found, installing...", args.package);
        eprintln!(
            "{} Tool {} not found, installing...",
            "●".green().bold(),
            args.package.cyan(),
        );

        install_tool(&args.package, args.version.as_deref())?;

        if !script_path.exists() {
            // The package might expose a differently-named script.
            // Try to find any script.
            let scripts = find_tool_scripts(&tool_venv);
            if scripts.is_empty() {
                return Err(miette::miette!(
                    "Package '{}' does not provide any console scripts",
                    args.package
                ));
            }

            // Look for an exact match with the package name
            let script_name = scripts
                .iter()
                .find(|s| normalize_package_name(s) == normalized)
                .or_else(|| scripts.first())
                .cloned()
                .expect("scripts is non-empty after empty check");

            let actual_script = tool_bin_dir.join(&script_name);
            return run_script(&actual_script, &args.args);
        }
    }

    run_script(&script_path, &args.args)
}

fn run_script(script_path: &Path, args: &[String]) -> Result<()> {
    let status = std::process::Command::new(script_path)
        .args(args)
        .status()
        .map_err(|e| miette::miette!("failed to execute '{}': {}", script_path.display(), e))?;

    // Intentional: propagate child process exit code to caller, matching uvx behavior
    std::process::exit(status.code().unwrap_or(1));
}

fn cmd_tool_install(args: ToolInstallArgs) -> Result<()> {
    let (version, scripts) = install_tool(&args.package, args.version.as_deref())?;

    eprintln!(
        "{} Installed {} {}",
        "✓".green().bold(),
        args.package.cyan(),
        version.green(),
    );

    if scripts.is_empty() {
        eprintln!("  {} No console scripts found.", "⚠".yellow().bold(),);
    } else {
        eprintln!(
            "  {} Available commands: {}",
            "→".dimmed(),
            scripts.join(", ").cyan(),
        );
    }

    Ok(())
}

fn cmd_tool_list() -> Result<()> {
    let tdir = tools_dir();

    if !tdir.exists() {
        return Ok(());
    }

    let mut entries: Vec<_> = std::fs::read_dir(&tdir)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read tools directory {}", tdir.display()))?
        .flatten()
        .filter(|e| e.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
        .collect();

    entries.sort_by_key(|e| e.file_name());

    if entries.is_empty() {
        return Ok(());
    }

    for entry in entries {
        let tool_name = entry.file_name().to_string_lossy().to_string();
        let tool_venv = entry.path();

        // Check if the venv is valid
        if !umbral_venv::is_venv(&tool_venv) {
            continue;
        }

        // Read version from site-packages
        let version = if let Some(sp) = umbral_venv::venv_site_packages(&tool_venv) {
            let installed = umbral_installer::scan_installed(&sp).unwrap_or_default();
            installed
                .iter()
                .find(|p| normalize_package_name(&p.name) == tool_name)
                .map(|p| p.version.clone())
                .unwrap_or_else(|| "unknown".to_string())
        } else {
            "unknown".to_string()
        };

        let scripts = find_tool_scripts(&tool_venv);
        let scripts_str = if scripts.is_empty() {
            String::new()
        } else {
            format!(" ({})", scripts.join(", "))
        };

        eprintln!(
            "  {} {}{}",
            tool_name.cyan(),
            version.green(),
            scripts_str.dimmed(),
        );
    }

    Ok(())
}

fn cmd_tool_uninstall(args: ToolUninstallArgs) -> Result<()> {
    let tdir = tools_dir();
    let bdir = tools_bin_dir();
    let normalized = normalize_package_name(&args.package);
    let tool_venv = tdir.join(&normalized);

    if !tool_venv.exists() {
        return Err(miette::miette!(
            "Tool '{}' is not installed (no venv at {})",
            args.package,
            tool_venv.display(),
        ));
    }

    // Find scripts before removing venv so we can clean up symlinks
    let scripts = find_tool_scripts(&tool_venv);

    // Remove the tool's venv
    std::fs::remove_dir_all(&tool_venv)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to remove tool venv at {}", tool_venv.display()))?;

    // Remove symlinks from bin dir that pointed into this tool's venv
    // Canonical path comparison prevents symlink traversal attacks
    for script in &scripts {
        let link = bdir.join(script);
        if let Ok(target) = link.read_link() {
            // Use canonical paths to prevent symlink traversal
            if let (Ok(target_canon), Ok(venv_canon)) = (
                std::fs::canonicalize(&target),
                std::fs::canonicalize(&tool_venv),
            ) {
                if target_canon.starts_with(&venv_canon) {
                    let _ = std::fs::remove_file(&link);
                }
            }
        } else if !link.exists() {
            // Dangling symlink (venv was removed), clean it up
            let _ = std::fs::remove_file(&link);
        }
    }

    eprintln!("{} Uninstalled {}", "✓".green().bold(), args.package.cyan(),);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tools_dir_default() {
        // When the env var is not set, tools_dir() should return a path
        // containing "umbral/tools" regardless of what other tests do.
        // We just verify the function returns a reasonable path — don't
        // mutate env vars since tests run in parallel.
        let dir = tools_dir();
        let dir_str = dir.to_string_lossy();
        // Either the env var is set and we get that, or it defaults to
        // a HOME-based or /tmp path containing "umbral"
        assert!(
            dir_str.contains("umbral") || dir_str.contains("tools"),
            "tools_dir should contain 'umbral' or 'tools', got: {}",
            dir_str
        );
    }

    #[test]
    fn test_tools_bin_dir_default() {
        let dir = tools_bin_dir();
        let dir_str = dir.to_string_lossy();
        // Should end with /bin or contain bin
        assert!(
            dir_str.ends_with("/bin") || dir_str.ends_with("\\bin") || dir_str.contains("bin"),
            "tools_bin_dir should contain 'bin', got: {}",
            dir_str
        );
    }

    #[test]
    fn test_find_tool_scripts() {
        let tmp = tempfile::tempdir().unwrap();
        let venv = tmp.path().join("test-venv");
        let bin_name = if cfg!(windows) { "Scripts" } else { "bin" };
        let bin = venv.join(bin_name);
        std::fs::create_dir_all(&bin).unwrap();

        // Create some fake scripts
        std::fs::write(bin.join("mytool"), "#!/bin/sh\necho hello").unwrap();
        std::fs::write(bin.join("another-tool"), "#!/bin/sh\necho hi").unwrap();

        // Create files that should be excluded
        std::fs::write(bin.join("python3.12"), "").unwrap();
        std::fs::write(bin.join("python3"), "").unwrap();
        std::fs::write(bin.join("python"), "").unwrap();
        std::fs::write(bin.join("pip"), "").unwrap();
        std::fs::write(bin.join("pip3"), "").unwrap();
        std::fs::write(bin.join("activate"), "").unwrap();
        std::fs::write(bin.join("activate.fish"), "").unwrap();
        std::fs::write(bin.join("activate.ps1"), "").unwrap();
        std::fs::write(bin.join("activate.bat"), "").unwrap();
        std::fs::write(bin.join(".hidden"), "").unwrap();
        std::fs::write(bin.join("pyvenv.cfg"), "").unwrap();

        let scripts = find_tool_scripts(&venv);
        assert!(scripts.contains(&"mytool".to_string()));
        assert!(scripts.contains(&"another-tool".to_string()));
        assert!(!scripts.contains(&"python3.12".to_string()));
        assert!(!scripts.contains(&"python3".to_string()));
        assert!(!scripts.contains(&"python".to_string()));
        assert!(!scripts.contains(&"pip".to_string()));
        assert!(!scripts.contains(&"pip3".to_string()));
        assert!(!scripts.contains(&"activate".to_string()));
        assert!(!scripts.contains(&"activate.fish".to_string()));
        assert!(!scripts.contains(&"activate.ps1".to_string()));
        assert!(!scripts.contains(&"activate.bat".to_string()));
        assert!(!scripts.contains(&".hidden".to_string()));
        assert!(!scripts.contains(&"pyvenv.cfg".to_string()));
        assert_eq!(scripts.len(), 2);
    }

    #[test]
    fn test_find_tool_scripts_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let venv = tmp.path().join("empty-venv");
        let bin = venv.join("bin");
        std::fs::create_dir_all(&bin).unwrap();

        let scripts = find_tool_scripts(&venv);
        assert!(scripts.is_empty());
    }

    #[test]
    fn test_find_tool_scripts_nonexistent() {
        let scripts = find_tool_scripts(Path::new("/nonexistent/venv"));
        assert!(scripts.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn test_uninstall_does_not_follow_external_symlinks() {
        // Create a temp dir structure simulating a tool venv
        let tmp = tempfile::tempdir().unwrap();
        let tool_venv = tmp.path().join("tools/mytool");
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(tool_venv.join("bin")).unwrap();
        std::fs::create_dir_all(&bin_dir).unwrap();

        // Create an external file that should NOT be affected
        let external_dir = tmp.path().join("external");
        std::fs::create_dir_all(&external_dir).unwrap();
        let external_file = external_dir.join("precious");
        std::fs::write(&external_file, "do not delete").unwrap();

        // Create a symlink in bin_dir pointing OUTSIDE the venv
        let malicious_link = bin_dir.join("mytool");
        std::os::unix::fs::symlink(&external_file, &malicious_link).unwrap();

        // Run the cleanup logic (same as cmd_tool_uninstall symlink removal)
        let link = &malicious_link;
        if let Ok(target) = link.read_link() {
            if let (Ok(target_canon), Ok(venv_canon)) = (
                std::fs::canonicalize(&target),
                std::fs::canonicalize(&tool_venv),
            ) {
                if target_canon.starts_with(&venv_canon) {
                    let _ = std::fs::remove_file(link);
                }
            }
        }

        // Verify the external symlink was NOT deleted
        assert!(
            malicious_link.exists(),
            "symlink pointing outside the venv should not be removed"
        );

        // Verify the external file is still intact
        assert!(
            external_file.exists(),
            "external file should not be affected"
        );
        assert_eq!(
            std::fs::read_to_string(&external_file).unwrap(),
            "do not delete"
        );
    }
}
