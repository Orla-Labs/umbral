# Contributing to Umbral

Thank you for your interest in contributing to Umbral! This guide will help you get started.

## Development Setup

### Prerequisites

- [Rust](https://rustup.rs/) (1.88 or later)
- Python 3.10+ (for integration tests)
- Git

### Getting Started

```bash
git clone https://github.com/moonshotideas/umbral.git
cd umbral
cargo build
cargo test --workspace
```

## Project Structure

Umbral is organized as a Cargo workspace with 9 crates:

| Crate | Tests | Description |
|---|---|---|
| `umbral-pep440` | 61 | PEP 440 version parsing, normalization, specifiers, ordering. Proptest coverage. |
| `umbral-pep508` | 97 | PEP 508 dependency parsing, marker evaluation (12 vars incl `extra`), `or(`/`and(` support |
| `umbral-pypi-client` | 57 | Async PyPI client (PEP 503/691/658/714), multi-index, platform tag detection, wiremock tests |
| `umbral-project` | 50 | pyproject.toml reader (PEP 621/518/735), `[tool.uv]` configuration parsing |
| `umbral-resolver` | 94 | PubGrub-based dependency resolver, live PyPI, error hints, marker filtering, scenario tests, constraint/override support |
| `umbral-lockfile` | 41 | **uv.lock format** compatible parser/writer, all source types, atomic writes |
| `umbral-venv` | 40 | Pure-Rust venv creation (~4ms), Python discovery, `.python-version` support, 4 activation scripts |
| `umbral-installer` | 60 | Wheel installer, RECORD compliance, console scripts, editable installs, path traversal protection |
| `umbral-cli` | 73 | CLI binary: 15 commands (init, build, add, remove, resolve, lock, venv, install, run, sync, python, publish, tool, pip, help) |

All crates live under `crates/` and share workspace-level dependency versions defined in the root `Cargo.toml`.

## Running Tests

```bash
# Run all tests (587 tests)
cargo test --workspace

# Run tests for a specific crate
cargo test -p umbral-pep440

# Run a specific test
cargo test -p umbral-project -- parse_full
```

## Code Style

Before submitting a PR, ensure your code passes formatting and lint checks:

```bash
cargo fmt --all
cargo clippy --workspace -- -D warnings
```

## Pull Request Process

1. **Fork** the repository and create a feature branch from `main`.
2. **Write tests** for any new functionality.
3. **Run the full test suite** — `cargo test --workspace` must pass.
4. **Run formatting and lints** — `cargo fmt --all` and `cargo clippy`.
5. **Open a PR** against `main` with a clear description of what changed and why.
6. A maintainer will review your PR. Address any feedback, then it will be merged.

Keep PRs focused — one logical change per PR. If your change is large, consider splitting it into smaller, reviewable pieces.

## Architecture

### uv Compatibility

Umbral is designed as a **drop-in replacement for uv**:

- Reads and writes `uv.lock` format
- Respects `[tool.uv]` configuration in `pyproject.toml`
- Matches uv's workflow: `init → add → run` (every command does everything in one step)

### Key Design Decisions

- **PubGrub resolver** with Python version as a virtual dependency (enables automatic backtracking)
- **Platform tag detection** for native wheel support (macOS/Linux/Windows)
- **`ensure_synced()`** — shared function that auto-resolves, auto-creates venv, auto-installs
- **Multi-index support** with fallback on 404

### PEP Compliance

PEP 440, 503, 508, 518, 621, 658, 691, 714, 735

## Issue Labels

| Label | Description |
|---|---|
| `good-first-issue` | Beginner-friendly issues — great starting point for new contributors |
| `bug` | Something isn't working correctly |
| `enhancement` | New feature or improvement to existing functionality |
| `rfc` | Major change requiring community discussion and maintainer vote |
| `docs` | Documentation improvements |
| `perf` | Performance-related work |

If you're new to the project, look for issues labeled `good-first-issue`.

## Where to Start

- Browse open issues, especially those labeled `good-first-issue`.
- Read through the crate you're interested in — each has a `lib.rs` with module-level documentation.
- Check the test suite to understand expected behavior.
- Ask questions in issue discussions — we're happy to help.

## License

By contributing to Umbral, you agree that your contributions will be licensed under the MIT OR Apache-2.0 license, matching the project's existing license terms.
