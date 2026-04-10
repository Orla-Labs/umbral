mod commands;

use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use miette::Result;
use owo_colors::OwoColorize;
use tracing_subscriber::EnvFilter;

use commands::add::AddArgs;
use commands::build::BuildArgs;
use commands::init::InitArgs;
use commands::install::InstallArgs;
use commands::pip::PipArgs;
use commands::publish::PublishArgs;
use commands::python::PythonArgs;
use commands::remove::RemoveArgs;
use commands::resolve::ResolveArgs;
use commands::run::RunArgs;
use commands::sync::SyncArgs;
use commands::tool::ToolArgs;
use commands::venv::VenvArgs;

/// umbral -- a fast, open-source Python package manager.
#[derive(Debug, Parser)]
#[command(
    name = "umbral",
    version,
    about = "A fast, open-source Python package manager",
    styles = styles(),
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Increase verbosity (-v, -vv, -vvv)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Decrease output (suppress warnings)
    #[arg(short, long, global = true)]
    quiet: bool,

    /// Control colored output
    #[arg(long, value_enum, default_value_t = ColorChoice::Auto, global = true)]
    color: ColorChoice,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize a new project with a pyproject.toml.
    Init(InitArgs),

    /// Build distributable artifacts (wheel and/or sdist) from the current project.
    Build(BuildArgs),

    /// Add dependencies to pyproject.toml, lock, and sync.
    Add(AddArgs),

    /// Remove dependencies from pyproject.toml, lock, and sync.
    Remove(RemoveArgs),

    /// Resolve project dependencies and generate a lockfile.
    Resolve(ResolveArgs),

    /// Resolve project dependencies and generate a lockfile (alias for `resolve`).
    Lock(ResolveArgs),

    /// Create a virtual environment.
    Venv(VenvArgs),

    /// Install packages into the virtual environment (alias for `sync`).
    Install(InstallArgs),

    /// Run a command inside the virtual environment (auto-syncs first).
    Run(RunArgs),

    /// Sync the virtual environment to match the lockfile exactly.
    Sync(SyncArgs),

    /// Manage Python interpreter installations.
    Python(PythonArgs),

    /// Publish distributions to PyPI.
    Publish(PublishArgs),

    /// Manage Python tools (uvx equivalent).
    Tool(ToolArgs),

    /// pip-compatible interface for installing packages directly into a venv.
    Pip(PipArgs),
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ColorChoice {
    Auto,
    Always,
    Never,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    // Configure color output
    match cli.color {
        ColorChoice::Always => owo_colors::set_override(true),
        ColorChoice::Never => owo_colors::set_override(false),
        ColorChoice::Auto => {} // default behavior
    }

    // Set up tracing/logging based on verbosity
    init_tracing(cli.verbose, cli.quiet);

    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(report) => {
            eprintln!("{} {:?}", "error:".red().bold(), report);
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Init(args) => commands::init::cmd_init(args),
        Command::Build(args) => commands::build::cmd_build(args),
        Command::Add(args) => commands::add::cmd_add(args),
        Command::Remove(args) => commands::remove::cmd_remove(args),
        Command::Resolve(args) => commands::resolve::cmd_resolve(args),
        Command::Lock(args) => commands::resolve::cmd_resolve(args),
        Command::Venv(args) => commands::venv::cmd_venv(args),
        Command::Install(args) => commands::install::cmd_install(args),
        Command::Run(args) => commands::run::cmd_run(args),
        Command::Sync(args) => commands::sync::cmd_sync(args),
        Command::Publish(args) => commands::publish::cmd_publish(args),
        Command::Python(args) => commands::python::cmd_python(args),
        Command::Tool(args) => commands::tool::cmd_tool(args),
        Command::Pip(args) => commands::pip::cmd_pip(args),
    }
}

/// Initialize tracing subscriber based on verbosity flags.
fn init_tracing(verbose: u8, quiet: bool) {
    let level = if quiet {
        "error"
    } else {
        match verbose {
            0 => "warn",
            1 => "info",
            2 => "debug",
            _ => "trace",
        }
    };

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .init();
}

/// Custom clap styles for colored help output.
fn styles() -> clap::builder::Styles {
    use clap::builder::styling::{AnsiColor, Color, Style};

    clap::builder::Styles::styled()
        .header(
            Style::new()
                .bold()
                .fg_color(Some(Color::Ansi(AnsiColor::Green))),
        )
        .usage(
            Style::new()
                .bold()
                .fg_color(Some(Color::Ansi(AnsiColor::Green))),
        )
        .literal(Style::new().fg_color(Some(Color::Ansi(AnsiColor::Cyan))))
        .placeholder(Style::new().fg_color(Some(Color::Ansi(AnsiColor::Cyan))))
}
