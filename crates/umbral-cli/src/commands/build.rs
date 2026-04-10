use std::path::PathBuf;
use std::time::Instant;

use clap::Parser;
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use tracing::info;

#[derive(Debug, Parser)]
pub struct BuildArgs {
    /// Output directory for built distributions
    #[arg(short, long, default_value = "dist")]
    pub output_dir: PathBuf,

    /// Build only the wheel
    #[arg(long)]
    pub wheel: bool,

    /// Build only the sdist
    #[arg(long)]
    pub sdist: bool,

    /// Python interpreter to use for building
    #[arg(long)]
    pub python: Option<PathBuf>,
}

pub fn cmd_build(args: BuildArgs) -> Result<()> {
    let started = Instant::now();
    let project_dir = std::env::current_dir().into_diagnostic()?;
    let pyproject_path = project_dir.join("pyproject.toml");

    eprintln!(
        "{} {} project...",
        "\u{25cf}".green().bold(),
        "Building".bold(),
    );

    // Read and parse pyproject.toml
    let content = std::fs::read_to_string(&pyproject_path)
        .into_diagnostic()
        .map_err(|_| miette::miette!("no pyproject.toml found in current directory"))?;
    let project =
        umbral_project::PyProject::from_str(&content).map_err(|e| miette::miette!("{}", e))?;

    // Get build system config (defaults to setuptools per PEP 518)
    let build_system = project.build_system_or_default();

    let backend = build_system
        .build_backend
        .ok_or_else(|| miette::miette!("no build-backend specified in [build-system]"))?;

    // Find Python interpreter
    let python = if let Some(p) = args.python {
        p
    } else {
        let venv_python = if cfg!(windows) {
            project_dir.join(".venv").join("Scripts").join("python.exe")
        } else {
            project_dir.join(".venv").join("bin").join("python3")
        };
        if venv_python.exists() {
            venv_python
        } else if cfg!(windows) {
            PathBuf::from("python")
        } else {
            PathBuf::from("python3")
        }
    };

    // Create output directory
    std::fs::create_dir_all(&args.output_dir).into_diagnostic()?;

    let config = umbral_installer::build::BuildConfig {
        python: python.clone(),
        build_backend: backend.clone(),
        requires: build_system.requires.clone(),
        backend_path: build_system.backend_path.clone(),
    };

    let build_both = !args.wheel && !args.sdist;

    // Build wheel (unless --sdist only)
    if args.wheel || build_both {
        info!("Building wheel...");
        let wheel_path = umbral_installer::build::build_wheel_from_source(
            &project_dir,
            &args.output_dir,
            &config,
        )
        .map_err(|e| miette::miette!("wheel build failed: {}", e))?;

        eprintln!(
            "  {} Built wheel: {}",
            "\u{2192}".dimmed(),
            wheel_path.display().to_string().cyan(),
        );
    }

    // Build sdist (unless --wheel only)
    if args.sdist || build_both {
        info!("Building sdist...");
        let sdist_path = umbral_installer::build::build_sdist_from_source(
            &project_dir,
            &args.output_dir,
            &config,
        )
        .map_err(|e| miette::miette!("sdist build failed: {}", e))?;

        eprintln!(
            "  {} Built sdist: {}",
            "\u{2192}".dimmed(),
            sdist_path.display().to_string().cyan(),
        );
    }

    let elapsed = started.elapsed();
    eprintln!(
        "\n{} Build complete in {:.1?}",
        "\u{2713}".green().bold(),
        elapsed,
    );

    Ok(())
}
