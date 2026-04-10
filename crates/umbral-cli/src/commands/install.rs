use std::path::PathBuf;

use clap::Parser;
use miette::Result;
use owo_colors::OwoColorize;

use super::{DEFAULT_LOCKFILE, DEFAULT_PROJECT, DEFAULT_VENV};

#[derive(Debug, Parser)]
pub struct InstallArgs {
    /// Path to pyproject.toml
    #[arg(long, default_value = DEFAULT_PROJECT)]
    project: PathBuf,

    /// Path to the lockfile
    #[arg(long, default_value = DEFAULT_LOCKFILE)]
    lockfile: PathBuf,

    /// Path to the virtual environment
    #[arg(long, default_value = DEFAULT_VENV)]
    venv: PathBuf,

    /// Base package index URL (overrides lockfile metadata)
    #[arg(long)]
    index_url: Option<String>,
}

pub fn cmd_install(args: InstallArgs) -> Result<()> {
    eprintln!(
        "{} `umbral install` is an alias for `umbral sync`.",
        "hint:".dimmed(),
    );

    super::ensure_synced(
        &args.project,
        &args.lockfile,
        &args.venv,
        args.index_url.as_deref(),
    )
}
