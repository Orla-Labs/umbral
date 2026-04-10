use std::path::PathBuf;

use clap::Parser;
use miette::Result;

use super::{discover_workspace, DEFAULT_LOCKFILE, DEFAULT_PROJECT, DEFAULT_VENV};

#[derive(Debug, Parser)]
pub struct SyncArgs {
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

pub fn cmd_sync(args: SyncArgs) -> Result<()> {
    let project_dir = args
        .project
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));

    // If in a workspace, read the lockfile from the workspace root.
    let lockfile_path = if let Some(ws) = discover_workspace(project_dir) {
        tracing::info!(
            workspace_root = %ws.root.display(),
            "detected workspace, using workspace root lockfile"
        );
        ws.root.join("uv.lock")
    } else {
        args.lockfile.clone()
    };

    super::ensure_synced(
        &args.project,
        &lockfile_path,
        &args.venv,
        args.index_url.as_deref(),
    )
}
