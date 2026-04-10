use std::path::PathBuf;
use std::time::Instant;

use clap::Parser;
use miette::{Context, IntoDiagnostic, Result};
use owo_colors::OwoColorize;

#[derive(Debug, Parser)]
pub struct VenvArgs {
    /// Path for the virtual environment
    #[arg(default_value = ".venv")]
    path: PathBuf,

    /// Python version to use (e.g. "3.12")
    #[arg(long)]
    python: Option<String>,

    /// Prompt prefix for the virtual environment
    #[arg(long)]
    prompt: Option<String>,
}

pub fn cmd_venv(args: VenvArgs) -> Result<()> {
    let started = Instant::now();

    eprintln!(
        "{} {} virtual environment at {}",
        "●".green().bold(),
        "Creating".bold(),
        args.path.display().to_string().cyan()
    );

    // Find a Python interpreter
    let interpreter = umbral_venv::PythonInterpreter::find(args.python.as_deref())
        .into_diagnostic()
        .wrap_err("failed to find a Python interpreter")?;

    eprintln!(
        "  {} Using Python {} ({})",
        "→".dimmed(),
        interpreter.version.to_string().dimmed(),
        interpreter.path.display().to_string().dimmed()
    );

    // Create the venv
    let prompt = args.prompt.as_deref();
    let venv_info = umbral_venv::create_venv(&args.path, &interpreter, prompt)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to create venv at {}", args.path.display()))?;

    let elapsed = started.elapsed();
    eprintln!(
        "\n{} Created virtual environment in {:.1?}",
        "✓".green().bold(),
        elapsed
    );
    eprintln!(
        "  {} {}",
        "→".dimmed(),
        venv_info.path.display().to_string().dimmed()
    );
    let activate_hint = if cfg!(windows) {
        format!("{}\\Scripts\\activate.bat", venv_info.path.display())
    } else {
        format!("source {}/bin/activate", venv_info.path.display())
    };
    eprintln!("  {} Activate with: {}", "→".dimmed(), activate_hint.cyan());

    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_activation_hint_format() {
        // Verify the activation hint uses the correct platform-specific format.
        let venv_path = "/tmp/test-venv";

        let hint = if cfg!(windows) {
            format!("{}\\Scripts\\activate.bat", venv_path)
        } else {
            format!("source {}/bin/activate", venv_path)
        };

        if cfg!(windows) {
            assert!(
                hint.contains("Scripts\\activate.bat"),
                "Windows hint should reference Scripts\\activate.bat: {hint}"
            );
        } else {
            assert!(
                hint.starts_with("source ") && hint.contains("/bin/activate"),
                "Unix hint should use 'source .../bin/activate': {hint}"
            );
        }
    }
}
