use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use clap::{Args, Subcommand};
use miette::{Context, IntoDiagnostic, Result};
use owo_colors::OwoColorize;

use super::{
    detect_python_version, dirs_cache_dir, download_and_install_packages, normalize_package_name,
    remove_installed_package,
};

// ── CLI argument types ─────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct PipArgs {
    #[command(subcommand)]
    pub command: PipCommand,
}

#[derive(Debug, Subcommand)]
pub enum PipCommand {
    /// Install packages into the active environment
    Install(PipInstallArgs),
    /// List installed packages
    List,
    /// Output installed packages in requirements format
    Freeze,
    /// Uninstall packages
    Uninstall(PipUninstallArgs),
    /// Compile requirements.in to requirements.txt (pip-compile)
    Compile(PipCompileArgs),
}

#[derive(Debug, Args)]
pub struct PipInstallArgs {
    /// Packages to install (e.g., "requests>=2.0" "flask")
    pub packages: Vec<String>,
    /// Requirements file
    #[arg(short, long)]
    pub requirement: Option<PathBuf>,
    /// Target virtual environment (default: .venv or VIRTUAL_ENV)
    #[arg(long)]
    pub target: Option<PathBuf>,
    /// Extra index URL
    #[arg(long)]
    pub extra_index_url: Option<String>,
}

#[derive(Debug, Args)]
pub struct PipUninstallArgs {
    /// Packages to uninstall
    pub packages: Vec<String>,
}

#[derive(Debug, Args)]
pub struct PipCompileArgs {
    /// Input requirements file (default: requirements.in)
    pub src: Option<PathBuf>,
    /// Output file (default: requirements.txt)
    #[arg(short, long)]
    pub output_file: Option<PathBuf>,
    /// Python version to compile for
    #[arg(long)]
    pub python_version: Option<String>,
}

// ── Dispatch ───────────────────────────────────────────────────────

pub fn cmd_pip(args: PipArgs) -> Result<()> {
    match args.command {
        PipCommand::Install(install_args) => cmd_pip_install(install_args),
        PipCommand::List => cmd_pip_list(),
        PipCommand::Freeze => cmd_pip_freeze(),
        PipCommand::Uninstall(uninstall_args) => cmd_pip_uninstall(uninstall_args),
        PipCommand::Compile(compile_args) => cmd_pip_compile(compile_args),
    }
}

// ── Helper: find active venv ───────────────────────────────────────

fn find_active_venv(target: Option<&Path>) -> Result<PathBuf> {
    // 1. Explicit --target
    if let Some(t) = target {
        if !umbral_venv::is_venv(t) {
            return Err(miette::miette!(
                "specified target {} is not a valid virtual environment",
                t.display()
            ));
        }
        return Ok(t.to_path_buf());
    }
    // 2. VIRTUAL_ENV env var
    if let Ok(venv) = std::env::var("VIRTUAL_ENV") {
        let venv_path = PathBuf::from(&venv);
        if umbral_venv::is_venv(&venv_path) {
            return Ok(venv_path);
        }
    }
    // 3. .venv in current directory
    let dot_venv = std::env::current_dir()
        .into_diagnostic()
        .wrap_err("failed to determine current directory")?
        .join(".venv");
    if dot_venv.exists() && umbral_venv::is_venv(&dot_venv) {
        return Ok(dot_venv);
    }
    Err(miette::miette!(
        "no virtual environment found. Create one with `umbral venv` or activate an existing one"
    ))
}

/// Return the Python binary path within a venv.
fn venv_python(venv_path: &Path) -> PathBuf {
    if cfg!(windows) {
        venv_path.join("Scripts").join("python.exe")
    } else {
        venv_path.join("bin").join("python")
    }
}

// ── pip install ────────────────────────────────────────────────────

fn cmd_pip_install(args: PipInstallArgs) -> Result<()> {
    let started = Instant::now();

    // Collect package specs from positional args and -r file
    let mut package_specs: Vec<String> = args.packages.clone();

    if let Some(ref req_file) = args.requirement {
        let content = std::fs::read_to_string(req_file)
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to read requirements file {}", req_file.display()))?;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('-') {
                continue;
            }
            package_specs.push(trimmed.to_string());
        }
    }

    if package_specs.is_empty() {
        return Err(miette::miette!(
            "no packages specified. Provide package names or use -r <requirements.txt>"
        ));
    }

    let venv_path = find_active_venv(args.target.as_deref())?;

    eprintln!(
        "{} {} packages into {}",
        "●".green().bold(),
        "Installing".bold(),
        venv_path.display().to_string().cyan(),
    );

    // Parse each spec as a PEP 508 requirement
    let mut requirements: Vec<umbral_pep508::Requirement> = Vec::new();
    for spec in &package_specs {
        let req = umbral_pep508::Requirement::parse(spec)
            .map_err(|e| miette::miette!("failed to parse requirement '{}': {}", spec, e))?;
        requirements.push(req);
    }

    // Resolve all packages using the resolver
    let index_url_str = args
        .extra_index_url
        .as_deref()
        .unwrap_or("https://pypi.org/simple/");

    let rt = tokio::runtime::Runtime::new()
        .into_diagnostic()
        .wrap_err("failed to create async runtime")?;

    let index_url: url::Url = index_url_str
        .parse()
        .into_diagnostic()
        .wrap_err("invalid index URL")?;

    let cache_dir = dirs_cache_dir();
    let pypi_cache_dir = cache_dir.join("pypi");

    let client = Arc::new(
        umbral_pypi_client::SimpleApiClient::new(index_url, pypi_cache_dir)
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

    eprintln!("  {} Resolving dependencies...", "→".dimmed(),);

    let resolution = umbral_resolver::resolve(source, config, requirements)
        .map_err(|e| miette::miette!("{}", e))?;

    // Collect resolved (name, version) pairs
    let packages_to_install: Vec<(String, String)> = resolution
        .packages
        .values()
        .map(|pkg| (pkg.name.as_str().to_string(), pkg.version.to_string()))
        .collect();

    eprintln!(
        "  {} Resolved {} package(s)",
        "✓".green().bold(),
        packages_to_install.len(),
    );

    // Download and install
    let site_packages = umbral_venv::venv_site_packages(&venv_path).ok_or_else(|| {
        miette::miette!("could not find site-packages in {}", venv_path.display())
    })?;

    let python_path = venv_python(&venv_path);

    let pkg_refs: Vec<(&str, &str)> = packages_to_install
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();

    download_and_install_packages(
        &pkg_refs,
        index_url_str,
        &site_packages,
        &python_path,
        &cache_dir,
        "Installing",
        Some(&venv_path),
    )?;

    let elapsed = started.elapsed();

    // Print installed packages
    for (name, version) in &packages_to_install {
        eprintln!("  {} {} {}", "+".green(), name.cyan(), version.green());
    }

    eprintln!(
        "\n{} Installed {} package(s) in {:.1?}",
        "✓".green().bold(),
        packages_to_install.len(),
        elapsed,
    );

    Ok(())
}

// ── pip list ───────────────────────────────────────────────────────

fn cmd_pip_list() -> Result<()> {
    let venv_path = find_active_venv(None)?;
    let site_packages = umbral_venv::venv_site_packages(&venv_path).ok_or_else(|| {
        miette::miette!("could not find site-packages in {}", venv_path.display())
    })?;

    let mut installed = umbral_installer::scan_installed(&site_packages)
        .into_diagnostic()
        .wrap_err("failed to scan installed packages")?;

    installed.sort_by(|a, b| normalize_package_name(&a.name).cmp(&normalize_package_name(&b.name)));

    // Print as a table
    println!("{:<40} {}", "Package".bold(), "Version".bold());
    println!("{:<40} {}", "-".repeat(40), "-".repeat(20));
    for pkg in &installed {
        println!("{:<40} {}", pkg.name, pkg.version);
    }

    Ok(())
}

// ── pip freeze ─────────────────────────────────────────────────────

fn cmd_pip_freeze() -> Result<()> {
    let venv_path = find_active_venv(None)?;
    let site_packages = umbral_venv::venv_site_packages(&venv_path).ok_or_else(|| {
        miette::miette!("could not find site-packages in {}", venv_path.display())
    })?;

    let mut installed = umbral_installer::scan_installed(&site_packages)
        .into_diagnostic()
        .wrap_err("failed to scan installed packages")?;

    installed.sort_by(|a, b| normalize_package_name(&a.name).cmp(&normalize_package_name(&b.name)));

    for pkg in &installed {
        println!("{}=={}", normalize_package_name(&pkg.name), pkg.version);
    }

    Ok(())
}

// ── pip uninstall ──────────────────────────────────────────────────

fn cmd_pip_uninstall(args: PipUninstallArgs) -> Result<()> {
    if args.packages.is_empty() {
        return Err(miette::miette!("no packages specified to uninstall"));
    }

    let venv_path = find_active_venv(None)?;
    let site_packages = umbral_venv::venv_site_packages(&venv_path).ok_or_else(|| {
        miette::miette!("could not find site-packages in {}", venv_path.display())
    })?;

    let installed = umbral_installer::scan_installed(&site_packages)
        .into_diagnostic()
        .wrap_err("failed to scan installed packages")?;

    let mut removed_count = 0;

    for pkg_name in &args.packages {
        let normalized = normalize_package_name(pkg_name);
        let found = installed
            .iter()
            .find(|p| normalize_package_name(&p.name) == normalized);

        match found {
            Some(pkg) => {
                remove_installed_package(pkg, &site_packages)?;
                eprintln!("  {} {} {}", "-".red(), pkg.name.cyan(), pkg.version.red(),);
                removed_count += 1;
            }
            None => {
                eprintln!(
                    "  {} package '{}' is not installed",
                    "⚠".yellow().bold(),
                    pkg_name,
                );
            }
        }
    }

    eprintln!(
        "\n{} Uninstalled {} package(s)",
        "✓".green().bold(),
        removed_count,
    );

    Ok(())
}

// ── pip compile ────────────────────────────────────────────────────

fn cmd_pip_compile(args: PipCompileArgs) -> Result<()> {
    let started = Instant::now();

    let src_path = args.src.unwrap_or_else(|| PathBuf::from("requirements.in"));

    let output_path = args
        .output_file
        .unwrap_or_else(|| PathBuf::from("requirements.txt"));

    let content = std::fs::read_to_string(&src_path)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read {}", src_path.display()))?;

    // Parse requirements from the input file
    let mut requirements: Vec<umbral_pep508::Requirement> = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('-') {
            continue;
        }
        let req = umbral_pep508::Requirement::parse(trimmed)
            .map_err(|e| miette::miette!("failed to parse requirement '{}': {}", trimmed, e))?;
        requirements.push(req);
    }

    if requirements.is_empty() {
        return Err(miette::miette!(
            "no requirements found in {}",
            src_path.display()
        ));
    }

    eprintln!(
        "{} {} {} requirement(s) from {}",
        "●".green().bold(),
        "Compiling".bold(),
        requirements.len(),
        src_path.display().to_string().cyan(),
    );

    // Resolve against PyPI
    let detected_version = detect_python_version();
    let python_version_str = args.python_version.as_deref().unwrap_or(&detected_version);

    let rt = tokio::runtime::Runtime::new()
        .into_diagnostic()
        .wrap_err("failed to create async runtime")?;

    let index_url: url::Url = "https://pypi.org/simple/"
        .parse()
        .into_diagnostic()
        .wrap_err("invalid index URL")?;

    let cache_dir = dirs_cache_dir();
    let pypi_cache_dir = cache_dir.join("pypi");

    let client = Arc::new(
        umbral_pypi_client::SimpleApiClient::new(index_url, pypi_cache_dir)
            .into_diagnostic()
            .wrap_err("failed to create PyPI client")?,
    );

    let source = umbral_resolver::LivePypiSource::new(Arc::clone(&client), rt.handle().clone());

    let python_version: umbral_pep440::Version = python_version_str
        .parse()
        .into_diagnostic()
        .wrap_err("invalid --python-version")?;

    let config = umbral_resolver::ResolverConfig {
        python_version,
        markers: None,
        pre_release_policy: umbral_resolver::PreReleasePolicy::Disallow,
    };

    let resolution = umbral_resolver::resolve(source, config, requirements.clone())
        .map_err(|e| miette::miette!("{}", e))?;

    // Build "via" reverse mapping: for each resolved package, which input
    // requirements caused it to appear?
    let input_names: std::collections::HashSet<String> = requirements
        .iter()
        .map(|r| normalize_package_name(r.name.as_str()))
        .collect();

    let mut via_map: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    for pkg in resolution.packages.values() {
        for (dep_name, _) in &pkg.dependencies {
            let normalized_dep = normalize_package_name(dep_name.as_str());
            let normalized_parent = normalize_package_name(pkg.name.as_str());
            via_map
                .entry(normalized_dep)
                .or_default()
                .push(normalized_parent);
        }
    }

    // Build sorted output
    let mut sorted_packages: Vec<_> = resolution.packages.values().collect();
    sorted_packages.sort_by_key(|p| normalize_package_name(p.name.as_str()));

    // Write output file
    let mut output = String::new();
    output.push_str("#\n");
    output.push_str("# This file is autogenerated by umbral pip compile\n");
    output.push_str("#\n");

    for pkg in &sorted_packages {
        let normalized = normalize_package_name(pkg.name.as_str());
        output.push_str(&format!("{}=={}\n", normalized, pkg.version));

        // Add "via" comments for transitive dependencies (not direct input)
        if !input_names.contains(&normalized) {
            if let Some(parents) = via_map.get(&normalized) {
                let mut unique_parents: Vec<_> = parents.iter().collect();
                unique_parents.sort();
                unique_parents.dedup();
                for parent in unique_parents {
                    output.push_str(&format!("    # via {}\n", parent));
                }
            }
        }
    }

    std::fs::write(&output_path, &output)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to write {}", output_path.display()))?;

    let elapsed = started.elapsed();
    eprintln!(
        "\n{} Wrote {} package(s) to {} in {:.1?}",
        "✓".green().bold(),
        sorted_packages.len(),
        output_path.display().to_string().cyan(),
        elapsed,
    );

    Ok(())
}

// ── Unit tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_active_venv_from_env() {
        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        let venv_path = tmp.path().join("test-venv");

        // Create a minimal venv structure so is_venv() returns true
        let interp = match umbral_venv::PythonInterpreter::find(None) {
            Ok(i) => i,
            Err(_) => return, // skip if no python
        };
        umbral_venv::create_venv(&venv_path, &interp, None).expect("failed to create test venv");

        // Temporarily set VIRTUAL_ENV
        let old = std::env::var("VIRTUAL_ENV").ok();
        std::env::set_var("VIRTUAL_ENV", &venv_path);

        let result = find_active_venv(None);
        assert!(result.is_ok(), "should find venv from VIRTUAL_ENV");
        assert_eq!(
            result.unwrap().canonicalize().unwrap(),
            venv_path.canonicalize().unwrap()
        );

        // Restore
        match old {
            Some(val) => std::env::set_var("VIRTUAL_ENV", val),
            None => std::env::remove_var("VIRTUAL_ENV"),
        }
    }

    #[test]
    fn test_find_active_venv_dot_venv() {
        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        let dot_venv = tmp.path().join(".venv");

        let interp = match umbral_venv::PythonInterpreter::find(None) {
            Ok(i) => i,
            Err(_) => return, // skip if no python
        };
        umbral_venv::create_venv(&dot_venv, &interp, None).expect("failed to create test venv");

        // Ensure VIRTUAL_ENV is not set
        let old = std::env::var("VIRTUAL_ENV").ok();
        std::env::remove_var("VIRTUAL_ENV");

        // Change to tmp dir so .venv is in "current directory"
        let old_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let result = find_active_venv(None);
        assert!(result.is_ok(), "should find .venv in current directory");

        // Restore
        std::env::set_current_dir(old_dir).unwrap();
        match old {
            Some(val) => std::env::set_var("VIRTUAL_ENV", val),
            None => std::env::remove_var("VIRTUAL_ENV"),
        }
    }

    #[test]
    fn test_find_active_venv_none() {
        // Ensure no venv is discoverable
        let old_venv = std::env::var("VIRTUAL_ENV").ok();
        std::env::remove_var("VIRTUAL_ENV");

        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        let old_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let result = find_active_venv(None);
        assert!(result.is_err(), "should error when no venv found");
        let err_msg = format!("{:?}", result.unwrap_err());
        assert!(
            err_msg.contains("no virtual environment found"),
            "error should mention no venv found, got: {}",
            err_msg
        );

        // Restore
        std::env::set_current_dir(old_dir).unwrap();
        match old_venv {
            Some(val) => std::env::set_var("VIRTUAL_ENV", val),
            None => std::env::remove_var("VIRTUAL_ENV"),
        }
    }
}
