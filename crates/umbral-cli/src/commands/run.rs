use std::path::PathBuf;

use clap::Args;
use miette::Result;

use super::{DEFAULT_LOCKFILE, DEFAULT_PROJECT, DEFAULT_VENV};

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Path to pyproject.toml
    #[arg(long, default_value = DEFAULT_PROJECT)]
    project: PathBuf,

    /// Path to the lockfile
    #[arg(long, default_value = DEFAULT_LOCKFILE)]
    lockfile: PathBuf,

    /// Path to the virtual environment
    #[arg(long, default_value = DEFAULT_VENV)]
    venv: PathBuf,

    /// Command to run inside the virtual environment
    #[arg(trailing_var_arg = true, required = true)]
    command: Vec<String>,
}

pub fn cmd_run(args: RunArgs) -> Result<()> {
    // Auto-sync: resolve if needed, create venv if needed, install/sync packages.
    super::ensure_synced(&args.project, &args.lockfile, &args.venv, None)?;

    let bin_dir = if cfg!(windows) {
        args.venv.join("Scripts")
    } else {
        args.venv.join("bin")
    };

    let path_separator = if cfg!(windows) { ";" } else { ":" };
    let path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}{}{}", bin_dir.display(), path_separator, path);

    let venv_abs = std::fs::canonicalize(&args.venv).unwrap_or_else(|_| args.venv.clone());

    let status = std::process::Command::new(&args.command[0])
        .args(&args.command[1..])
        .env("PATH", &new_path)
        .env("VIRTUAL_ENV", &venv_abs)
        .status()
        .map_err(|e| miette::miette!("Failed to run '{}': {}", args.command[0], e))?;

    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_args_defaults() {
        let args = RunArgs {
            project: PathBuf::from(DEFAULT_PROJECT),
            lockfile: PathBuf::from(DEFAULT_LOCKFILE),
            venv: PathBuf::from(DEFAULT_VENV),
            command: vec!["echo".to_string()],
        };
        assert_eq!(args.venv, PathBuf::from(".venv"));
        assert_eq!(args.lockfile, PathBuf::from("./uv.lock"));
        assert_eq!(args.project, PathBuf::from("./pyproject.toml"));
    }
}
