# Umbral

A fast, open-source Python package manager written in Rust. Drop-in replacement for [uv](https://github.com/astral-sh/uv).

> **Why?** OpenAI acquired Astral.sh in March 2026. Umbral is the community-owned alternative — compatible with `uv.lock` and `[tool.uv]` config, but built to be **better**: faster resolution, clearer errors, cleaner architecture, and governed by the community, not a corporation.

## Quick Start

```bash
# Install from source
cargo install --path crates/umbral-cli

# Create a new project
umbral init

# Add dependencies (auto-resolves, creates venv, installs)
umbral add requests flask

# Run your code
umbral run python -c "import flask; print(flask.__version__)"
```

That's it. No separate `resolve`, `venv`, or `install` steps needed.

### Build and Publish

```bash
# Build a wheel and sdist
umbral build

# Publish to PyPI
umbral publish --token pypi-xxxx
```

## Commands

| Command | Description |
|---------|-------------|
| `umbral init` | Create a new `pyproject.toml` |
| `umbral add <pkg>` | Add dependencies + auto-sync |
| `umbral remove <pkg>` | Remove dependencies + auto-sync |
| `umbral sync` | Sync environment to match lockfile (auto-resolves if stale) |
| `umbral run <cmd>` | Run a command in the project environment (auto-syncs) |
| `umbral lock` | Generate/update `uv.lock` |
| `umbral build` | Build wheel and/or sdist from current project |
| `umbral publish` | Publish distributions to PyPI |
| `umbral tool run <pkg>` | Run a Python tool (uvx equivalent) |
| `umbral tool install <pkg>` | Install a tool persistently |
| `umbral pip install <pkg>` | Install packages directly (pip-compatible) |
| `umbral python install 3.12` | Install a managed Python version |
| `umbral python list` | List available/installed Python versions |

### Flags

```bash
umbral add requests --dev           # Add to dev dependency group
umbral add pytest --group test      # Add to named dependency group
umbral lock --python-version 3.11   # Lock for specific Python version
umbral lock --extra-index-url URL   # Use additional package index
```

## uv Compatibility

Umbral is designed to work alongside or replace uv:

- **Reads and writes `uv.lock`** — switch between tools seamlessly
- **Respects `[tool.uv]` config** — index URLs, dev-dependencies, constraints, overrides
- **Same workflow** — `init → add → run` or `sync → run`

Existing uv projects work with Umbral without changes.

## Workspace Support

Umbral supports monorepo workspaces with shared lockfiles:

```toml
# root pyproject.toml
[tool.uv.workspace]
members = ["packages/*"]
```

All members share a single `uv.lock` at the workspace root.

## Architecture

9 Rust crates, ~25,500 lines of code, 587 tests.

```
umbral-pep440          PEP 440 version parsing/ordering
umbral-pep508          PEP 508 dependency specs + markers
umbral-pypi-client     PyPI API client + platform tags
umbral-project         pyproject.toml + [tool.uv] config
umbral-resolver        PubGrub dependency resolver
umbral-lockfile        uv.lock parser/writer
umbral-venv            Pure-Rust venv creation
umbral-installer       Wheel installer + editable installs + PEP 517 build isolation
umbral-cli             CLI binary (15 commands)
```

## Known Limitations

- **Single-platform lockfile is default** — use `--universal` for cross-platform resolution
- **No `uv pip` full compatibility** — basic subset implemented (install, list, freeze, uninstall, compile)

## Roadmap

### Current (0.4.0)
- All features through 0.4.0 are implemented and hardened
- 15 commands, 587 tests, comprehensive security audit complete
- Musllinux + manylinux_2_5 platform support, lockfile artifact URLs
- `.python-version` file support, constraint/override dependency enforcement
- Benchmarks framework, expanded Python catalog, HTTP timeouts + retry

### Next (0.5.0)
- Performance: parallel metadata prefetching during resolution
- Compatibility: `find-links`, named `[[tool.uv.index]]` support, `--frozen` flag
- Polish: improved CLI flag coverage, `--no-sync` for run, `--dry-run` for sync

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup and guidelines.

## Governance

Umbral is stewarded by [Moonshot Ideas](https://github.com/moonshotideas) with a commitment to transfer governance to an independent foundation at 1.0. See [GOVERNANCE.md](GOVERNANCE.md).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
