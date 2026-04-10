use clap::{Parser, Subcommand};
use miette::{IntoDiagnostic, Result, WrapErr};
use owo_colors::OwoColorize;

use umbral_venv::python_download;

/// Manage Python interpreter installations.
#[derive(Debug, Parser)]
pub struct PythonArgs {
    #[command(subcommand)]
    pub command: PythonCommands,
}

#[derive(Debug, Subcommand)]
pub enum PythonCommands {
    /// Install a Python version from python-build-standalone.
    Install {
        /// Python version to install (e.g. "3.12", "3.12.7", "3.11")
        version: String,
    },

    /// List installed and available Python versions.
    List,

    /// Remove a managed Python installation.
    Remove {
        /// Python version to remove (e.g. "3.12.7")
        version: String,
    },
}

pub fn cmd_python(args: PythonArgs) -> Result<()> {
    match args.command {
        PythonCommands::Install { version } => cmd_python_install(&version),
        PythonCommands::List => cmd_python_list(),
        PythonCommands::Remove { version } => cmd_python_remove(&version),
    }
}

fn cmd_python_install(version_request: &str) -> Result<()> {
    let install_dir = python_download::default_install_dir();

    // Check if a matching distribution exists
    let dist = python_download::find_distribution(version_request).ok_or_else(|| {
        miette::miette!(
            "No Python distribution found for '{}' on {}/{}",
            version_request,
            python_download::current_os(),
            python_download::current_arch()
        )
    })?;

    eprintln!(
        "{} {} Python {} for {}/{}",
        "●".green().bold(),
        "Installing".bold(),
        dist.version.cyan(),
        dist.os,
        dist.arch,
    );
    eprintln!(
        "  {} Install directory: {}",
        "→".dimmed(),
        install_dir.display().to_string().dimmed()
    );
    eprintln!("  {} {}", "→".dimmed(), dist.url.dimmed());

    // Build a tokio runtime for the async download pipeline
    let rt = tokio::runtime::Runtime::new()
        .into_diagnostic()
        .wrap_err("failed to create async runtime")?;

    let executable = rt
        .block_on(python_download::download_and_install(&dist, &install_dir))
        .into_diagnostic()
        .wrap_err("failed to download and install Python")?;

    eprintln!(
        "\n{} Installed Python {} at {}",
        "✓".green().bold(),
        dist.version.cyan(),
        install_dir
            .join(format!("python-{}", dist.version))
            .display()
            .to_string()
            .dimmed()
    );
    eprintln!(
        "  {} Executable: {}",
        "→".dimmed(),
        executable.display().to_string().dimmed()
    );

    Ok(())
}

fn cmd_python_list() -> Result<()> {
    let install_dir = python_download::default_install_dir();

    // Show installed versions
    let installed = python_download::list_installed(&install_dir);

    if installed.is_empty() {
        eprintln!(
            "{} No managed Python installations found",
            "●".yellow().bold(),
        );
        eprintln!(
            "  {} Install one with: {} {}",
            "→".dimmed(),
            "umbral python install".cyan(),
            "<version>".dimmed()
        );
    } else {
        eprintln!(
            "{} {} managed Python installation(s):",
            "●".green().bold(),
            installed.len()
        );
        for python in &installed {
            eprintln!(
                "  {} Python {} ({})",
                "→".dimmed(),
                python.version.cyan(),
                python.executable.display().to_string().dimmed()
            );
        }
    }

    // Show available versions
    let available = python_download::available_versions();
    let installed_versions: Vec<&str> = installed.iter().map(|p| p.version.as_str()).collect();

    let not_installed: Vec<_> = available
        .iter()
        .filter(|d| !installed_versions.contains(&d.version.as_str()))
        .collect();

    if !not_installed.is_empty() {
        eprintln!("\n{} Available for install:", "●".dimmed(),);
        for dist in &not_installed {
            eprintln!(
                "  {} Python {} ({}/{})",
                "→".dimmed(),
                dist.version,
                dist.os,
                dist.arch,
            );
        }
    }

    Ok(())
}

fn cmd_python_remove(version: &str) -> Result<()> {
    let install_dir = python_download::default_install_dir();

    eprintln!(
        "{} {} Python {}...",
        "●".yellow().bold(),
        "Removing".bold(),
        version.cyan()
    );

    python_download::remove_python(version, &install_dir).into_diagnostic()?;

    eprintln!("{} Removed Python {}", "✓".green().bold(), version.cyan());

    Ok(())
}
