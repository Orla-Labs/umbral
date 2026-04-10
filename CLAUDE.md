# CLAUDE.md

Umbral is a fast Python package manager written in Rust, designed as a drop-in replacement for uv.
It reads/writes `uv.lock` and respects `[tool.uv]` config so existing uv projects work without changes.

## Build & Test Commands

- `cargo build --workspace` -- build all crates
- `cargo test --workspace` -- run all tests (587 tests)
- `cargo clippy --workspace -- -D warnings` -- lint (must be zero warnings)
- `cargo fmt --all -- --check` -- format check
- `cargo test -p umbral-pep440` -- test single crate
- `cargo run -p umbral-cli -- <command>` -- run CLI locally

Rust 1.85+ required. Python 3.10+ needed for integration tests.

## Architecture

9 crates under `crates/`, ~25K lines of code. Dependency graph:

```
umbral-pep440          (leaf -- no internal deps)
umbral-pep508          -> pep440
umbral-pypi-client     -> pep440, pep508
umbral-project         -> pep440, pep508
umbral-resolver        -> pep440, pep508, pypi-client
umbral-lockfile        (leaf -- no internal deps)
umbral-venv            -> pep440
umbral-installer       -> pep440, pypi-client, venv
umbral-cli             -> all 8 crates above
```

| Crate | Responsibility |
|---|---|
| `umbral-pep440` | PEP 440 version parsing, normalization, specifiers, ordering |
| `umbral-pep508` | PEP 508 dependency specs with environment marker evaluation |
| `umbral-pypi-client` | Async PyPI client (Simple API + JSON API), platform tag detection |
| `umbral-project` | pyproject.toml reader (PEP 621/518/735), `[tool.uv]` config, workspace discovery |
| `umbral-resolver` | PubGrub-based dependency resolver with live PyPI source |
| `umbral-lockfile` | uv.lock compatible parser/writer with atomic file writes |
| `umbral-venv` | Pure-Rust virtual environment creation and Python discovery |
| `umbral-installer` | Wheel installer with RECORD compliance, console scripts, editable installs, PEP 517 build isolation |
| `umbral-cli` | CLI binary: init, build, add, remove, resolve, lock, venv, install, run, sync, python, publish, tool, pip |

## Conventions

- **Error handling**: `thiserror` for library error types, `miette` for CLI display with fancy diagnostics
- **Logging**: `tracing` crate, verbosity controlled by `-v`/`-vv`/`-vvv` flags
- **TOML editing**: `toml_edit` for format-preserving pyproject.toml modifications, `toml`/`serde` for read-only parsing
- **Async**: `tokio` runtime, `reqwest` for HTTP. CLI commands use `#[tokio::main]` or `block_in_place`
- **Testing**: Unit tests inline in `#[cfg(test)] mod tests`, wiremock for HTTP mocking, tempdir for filesystem tests, proptest for property-based testing in pep440, insta for snapshot tests
- **CLI**: `clap` derive macros, colored output via `owo-colors`/`anstream`

## Key Patterns

- **`ensure_synced()`**: The core auto-everything pattern. All mutating CLI commands (add, remove, run, sync) funnel through this -- it auto-resolves if lockfile is stale, auto-creates venv if missing, auto-installs if out of sync.
- **PubGrub integration**: Python version is modeled as a virtual PubGrub package for automatic backtracking. Extras are also virtual packages pinned to base version.
- **uv.lock compatibility**: Read/write uv's lockfile format. Use `toml_edit` for parsing (not serde) because uv.lock has quirky dotted subtable ordering.
- **Universal resolution**: `resolve --universal` forks on environment markers (os, Python version) and merges results with marker annotations. Produces a single lockfile valid across platforms.

## Key Files

- `crates/umbral-cli/src/main.rs` -- CLI entry, command dispatch
- `crates/umbral-cli/src/commands/mod.rs` -- shared pipeline: `download_and_install_packages()`, `ensure_synced()`
- `crates/umbral-resolver/src/provider.rs` -- PubGrub DependencyProvider implementation
- `crates/umbral-resolver/src/live.rs` -- Live PyPI source with metadata fetching
- `crates/umbral-lockfile/src/lib.rs` -- uv.lock format parser/writer (entire crate in one file)
- `crates/umbral-pypi-client/src/tags.rs` -- Platform tag detection and wheel compatibility scoring
- `crates/umbral-installer/src/build.rs` -- PEP 517 build isolation (sdist->wheel)
- `crates/umbral-project/src/workspace.rs` -- Workspace discovery and member resolution
- `crates/umbral-cli/src/commands/tool.rs` -- Tool management (uvx equivalent)
- `crates/umbral-cli/src/commands/pip.rs` -- pip compatibility interface
- `crates/umbral-cli/src/commands/publish.rs` -- PyPI publishing
- `crates/umbral-cli/src/commands/build.rs` -- Build command

## uv Compatibility

Umbral reads/writes `uv.lock` and respects `[tool.uv]` config in pyproject.toml. The goal is drop-in replacement -- existing uv projects should work without changes.

## PEP Compliance

PEP 440, 503, 508, 518, 621, 658, 691, 714, 735.

## License

Dual-licensed under MIT or Apache-2.0.
