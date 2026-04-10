//! pyproject.toml reader — PEP 621 / 518 / 735 compliant.
//!
//! Parses `pyproject.toml` into strongly-typed Rust structs, validates
//! field constraints, and supports dependency-group expansion.

mod dependency_groups;
mod validation;
pub mod workspace;

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;
use umbral_pep440::VersionSpecifiers;

pub use dependency_groups::expand_dependency_group;
pub use validation::validate;

// ── Errors ──────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum ProjectError {
    #[error("failed to read {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },

    #[error("{}", format_toml_error(.0))]
    Toml(#[from] toml::de::Error),

    #[error("validation error: {0}")]
    Validation(String),

    #[error("dependency group cycle detected: {0}")]
    DependencyGroupCycle(String),

    #[error("unknown dependency group: {0}")]
    UnknownDependencyGroup(String),
}

/// Format a `toml::de::Error` with explicit line/column callout.
///
/// The `toml` crate's error messages already contain `" at line X column Y"` in
/// their Display output, but this can be buried in longer messages. We extract
/// the span and prepend a clear location prefix so users can jump directly to
/// the right place in their editor.
fn format_toml_error(err: &toml::de::Error) -> String {
    let msg = err.message();
    match err.span() {
        Some(span) => {
            // The span gives byte offsets; the toml crate's Display already
            // includes line/column, but we can produce a cleaner format.
            // Since we don't have the source text here, use the inner message
            // and annotate with the span-derived info from Display.
            let full = err.to_string();
            // If the full display already contains "at line", use it as-is
            // with our prefix. Otherwise fall back to byte offset.
            if full.contains("at line") {
                format!("failed to parse pyproject.toml: {full}")
            } else {
                format!(
                    "failed to parse pyproject.toml (byte offset {}..{}): {msg}",
                    span.start, span.end
                )
            }
        }
        None => format!("failed to parse pyproject.toml: {msg}"),
    }
}

// ── Top-level type ──────────────────────────────────────────────────

/// Parsed representation of a `pyproject.toml` file.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PyProject {
    pub project: Option<ProjectTable>,
    pub build_system: Option<BuildSystem>,
    pub dependency_groups: Option<HashMap<String, Vec<DependencyGroupSpecifier>>>,
    pub tool: Option<ToolTable>,
}

impl FromStr for PyProject {
    type Err = ProjectError;

    fn from_str(content: &str) -> Result<Self, Self::Err> {
        let pyproject: PyProject = toml::from_str(content)?;

        if pyproject.project.is_none() {
            warn!("[project] table is missing from pyproject.toml");
        }

        validate(&pyproject)?;
        Ok(pyproject)
    }
}

impl PyProject {
    /// Read and parse a `pyproject.toml` from a file path.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, ProjectError> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path).map_err(|e| ProjectError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        Self::from_str(&content)
    }

    /// Parse a `pyproject.toml` from a string.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(content: &str) -> Result<Self, ProjectError> {
        <Self as FromStr>::from_str(content)
    }

    /// Return the `[tool.uv]` configuration, if present.
    pub fn tool_uv(&self) -> Option<&ToolUv> {
        self.tool.as_ref()?.uv.as_ref()
    }

    /// Return `[project.dependencies]` or an empty vec.
    pub fn all_dependencies(&self) -> Vec<String> {
        self.project
            .as_ref()
            .and_then(|p| p.dependencies.clone())
            .unwrap_or_default()
    }

    /// Return dependencies for a given optional-dependency extra.
    pub fn optional_dependencies(&self, extra: &str) -> Vec<String> {
        self.project
            .as_ref()
            .and_then(|p| p.optional_dependencies.as_ref())
            .and_then(|map| map.get(extra))
            .cloned()
            .unwrap_or_default()
    }

    /// Parse the `requires-python` field into a `VersionSpecifiers`.
    pub fn python_requires(&self) -> Option<Result<VersionSpecifiers, umbral_pep440::ParseError>> {
        self.project
            .as_ref()
            .and_then(|p| p.requires_python.as_deref())
            .map(|s| s.parse::<VersionSpecifiers>())
    }

    /// Expand a dependency group by name, recursively resolving
    /// `{include-group = "…"}` entries.  Returns flattened PEP 508
    /// requirement strings.
    pub fn expand_dependency_group(&self, name: &str) -> Result<Vec<String>, ProjectError> {
        let groups = self
            .dependency_groups
            .as_ref()
            .ok_or_else(|| ProjectError::UnknownDependencyGroup(name.to_string()))?;
        expand_dependency_group(groups, name)
    }

    /// Return the build system, defaulting to setuptools per PEP 518
    /// when the table is absent.
    pub fn build_system_or_default(&self) -> BuildSystem {
        self.build_system.clone().unwrap_or_else(|| BuildSystem {
            requires: vec!["setuptools".to_string()],
            build_backend: None,
            backend_path: None,
        })
    }
}

// ── [tool] ──────────────────────────────────────────────────────────

/// The `[tool]` table in `pyproject.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolTable {
    pub uv: Option<ToolUv>,
}

/// Parsed `[tool.uv]` configuration — covers the most important uv
/// settings so Umbral can act as a drop-in replacement.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ToolUv {
    /// Whether the project is managed by uv/umbral.
    pub managed: Option<bool>,

    /// Whether this is a Python package (`false` = virtual project).
    pub package: Option<bool>,

    /// Primary package index URL.
    pub index_url: Option<String>,

    /// Extra package index URLs.
    #[serde(default)]
    pub extra_index_url: Vec<String>,

    /// Development dependencies (legacy, prefer `[dependency-groups]`).
    #[serde(default)]
    pub dev_dependencies: Vec<String>,

    /// Default groups to install.
    #[serde(default)]
    pub default_groups: Vec<String>,

    /// Constraint dependencies (restrict versions without adding).
    #[serde(default)]
    pub constraint_dependencies: Vec<String>,

    /// Override dependencies (force specific versions).
    #[serde(default)]
    pub override_dependencies: Vec<String>,

    /// Resolution mode: `"highest"`, `"lowest"`, `"lowest-direct"`.
    pub resolution: Option<String>,

    /// Pre-release mode: `"disallow"`, `"allow"`, `"if-necessary"`, etc.
    pub prerelease: Option<String>,

    /// Target Python version for resolution.
    pub python_version: Option<String>,

    /// Link mode: `"hardlink"`, `"copy"`, `"symlink"`, `"clone"`.
    pub link_mode: Option<String>,

    /// Max concurrent downloads.
    pub concurrent_downloads: Option<u32>,

    /// Named indexes.
    #[serde(default)]
    pub index: Vec<NamedIndex>,

    /// Dependency sources (git, path, url, workspace, registry).
    #[serde(default)]
    pub sources: BTreeMap<String, DependencySource>,

    /// Workspace configuration.
    pub workspace: Option<WorkspaceConfig>,

    /// Environments to resolve for.
    #[serde(default)]
    pub environments: Vec<String>,
}

/// A named package index entry inside `[[tool.uv.index]]`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct NamedIndex {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub explicit: bool,
    #[serde(default)]
    pub default: bool,
}

/// A dependency source declared in `[tool.uv.sources]`.
///
/// Variants are ordered so that `#[serde(untagged)]` tries the most-specific
/// (most fields) shapes first.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum DependencySource {
    Git {
        git: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        rev: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        subdirectory: Option<String>,
    },
    Path {
        path: String,
        #[serde(default)]
        editable: bool,
    },
    Url {
        url: String,
    },
    Workspace {
        workspace: bool,
    },
    Registry {
        index: String,
    },
}

/// Workspace configuration inside `[tool.uv.workspace]`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct WorkspaceConfig {
    #[serde(default)]
    pub members: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

// ── PEP 621 helper types ───────────────────────────────────────────

/// Readme can be a simple path string or a table with file/content-type.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ReadmeField {
    Path(String),
    Table {
        #[serde(skip_serializing_if = "Option::is_none")]
        file: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(rename = "content-type", skip_serializing_if = "Option::is_none")]
        content_type: Option<String>,
    },
}

/// License can be an SPDX string or a table with text/file.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum LicenseField {
    Spdx(String),
    Table {
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        file: Option<String>,
    },
}

/// Person field for authors/maintainers.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct PersonField {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

// ── [project] ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ProjectTable {
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub dependencies: Option<Vec<String>>,
    pub optional_dependencies: Option<HashMap<String, Vec<String>>>,
    pub requires_python: Option<String>,
    pub dynamic: Option<Vec<String>>,
    pub scripts: Option<HashMap<String, String>>,
    pub gui_scripts: Option<HashMap<String, String>>,

    /// PEP 621: readme - can be a string path or a table {file="...", content-type="..."}
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readme: Option<ReadmeField>,

    /// PEP 621: license - can be a string (SPDX) or a table {text="..."} or {file="..."}
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<LicenseField>,

    /// PEP 621: license-files (glob patterns)
    #[serde(
        default,
        rename = "license-files",
        skip_serializing_if = "Option::is_none"
    )]
    pub license_files: Option<Vec<String>>,

    /// PEP 621: authors
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authors: Option<Vec<PersonField>>,

    /// PEP 621: maintainers
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maintainers: Option<Vec<PersonField>>,

    /// PEP 621: keywords
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keywords: Option<Vec<String>>,

    /// PEP 621: classifiers (trove classifiers)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classifiers: Option<Vec<String>>,

    /// PEP 621: urls (e.g., Homepage, Repository, Documentation)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urls: Option<HashMap<String, String>>,

    /// PEP 621: entry-points (groups of entry points beyond scripts/gui-scripts)
    #[serde(
        default,
        rename = "entry-points",
        skip_serializing_if = "Option::is_none"
    )]
    pub entry_points: Option<HashMap<String, HashMap<String, String>>>,
}

// ── [build-system] ──────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct BuildSystem {
    pub requires: Vec<String>,
    pub build_backend: Option<String>,
    pub backend_path: Option<Vec<String>>,
}

// ── [dependency-groups] (PEP 735) ───────────────────────────────────

/// Raw serde-friendly representation that handles both string and table
/// forms in the TOML array.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum DependencyGroupSpecifier {
    Requirement(String),
    IncludeGroup {
        #[serde(rename = "include-group")]
        include_group: String,
    },
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal() {
        let toml = r#"
[project]
name = "tiny"
"#;
        let p = PyProject::from_str(toml).unwrap();
        let proj = p.project.unwrap();
        assert_eq!(proj.name, "tiny");
        assert!(proj.version.is_none());
        assert!(proj.dependencies.is_none());
    }

    #[test]
    fn parse_full() {
        let toml = r#"
[project]
name = "myproject"
version = "0.1.0"
description = "A cool project"
requires-python = ">=3.10"
dependencies = [
    "requests>=2.28",
    "click>=8.0",
]
dynamic = ["classifiers"]

[project.optional-dependencies]
dev = ["pytest>=7.0", "mypy>=1.0"]
docs = ["sphinx>=6.0"]

[project.scripts]
mycli = "myproject.cli:main"

[build-system]
requires = ["hatchling"]
build-backend = "hatchling.build"

[dependency-groups]
test = ["pytest>=7.0", "coverage>=7.0"]
dev = [{include-group = "test"}, "mypy>=1.0"]
"#;
        let p = PyProject::from_str(toml).unwrap();
        let proj = p.project.as_ref().unwrap();
        assert_eq!(proj.name, "myproject");
        assert_eq!(proj.version.as_deref(), Some("0.1.0"));
        assert_eq!(proj.description.as_deref(), Some("A cool project"));
        assert_eq!(proj.requires_python.as_deref(), Some(">=3.10"));
        assert_eq!(
            proj.dependencies.as_ref().unwrap(),
            &["requests>=2.28", "click>=8.0"]
        );
        assert_eq!(
            proj.optional_dependencies.as_ref().unwrap()["dev"],
            vec!["pytest>=7.0", "mypy>=1.0"]
        );
        assert_eq!(
            proj.scripts.as_ref().unwrap()["mycli"],
            "myproject.cli:main"
        );

        let bs = p.build_system.as_ref().unwrap();
        assert_eq!(bs.requires, vec!["hatchling"]);
        assert_eq!(bs.build_backend.as_deref(), Some("hatchling.build"));

        // dependency groups
        let groups = p.dependency_groups.as_ref().unwrap();
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn all_dependencies_helper() {
        let toml = r#"
[project]
name = "x"
dependencies = ["a", "b"]
"#;
        let p = PyProject::from_str(toml).unwrap();
        assert_eq!(p.all_dependencies(), vec!["a", "b"]);
    }

    #[test]
    fn optional_deps_helper() {
        let toml = r#"
[project]
name = "x"

[project.optional-dependencies]
dev = ["pytest"]
"#;
        let p = PyProject::from_str(toml).unwrap();
        assert_eq!(p.optional_dependencies("dev"), vec!["pytest"]);
        assert!(p.optional_dependencies("nope").is_empty());
    }

    #[test]
    fn python_requires_parses() {
        let toml = r#"
[project]
name = "x"
requires-python = ">=3.10, <4"
"#;
        let p = PyProject::from_str(toml).unwrap();
        let specs = p.python_requires().unwrap().unwrap();
        assert!(!specs.is_empty());
    }

    #[test]
    fn missing_build_system_defaults_to_setuptools() {
        let toml = r#"
[project]
name = "legacy"
"#;
        let p = PyProject::from_str(toml).unwrap();
        assert!(p.build_system.is_none());
        let bs = p.build_system_or_default();
        assert_eq!(bs.requires, vec!["setuptools"]);
    }

    #[test]
    fn name_in_dynamic_is_rejected() {
        let toml = r#"
[project]
name = "bad"
dynamic = ["name"]
"#;
        let err = PyProject::from_str(toml).unwrap_err();
        assert!(matches!(err, ProjectError::Validation(_)));
    }

    #[test]
    fn static_and_dynamic_field_rejected() {
        let toml = r#"
[project]
name = "bad"
version = "1.0"
dynamic = ["version"]
"#;
        let err = PyProject::from_str(toml).unwrap_err();
        assert!(matches!(err, ProjectError::Validation(_)));
    }

    #[test]
    fn dependency_group_expansion() {
        let toml = r#"
[project]
name = "x"

[dependency-groups]
test = ["pytest>=7.0", "coverage>=7.0"]
dev = [{include-group = "test"}, "mypy>=1.0"]
"#;
        let p = PyProject::from_str(toml).unwrap();
        let expanded = p.expand_dependency_group("dev").unwrap();
        assert_eq!(expanded, vec!["pytest>=7.0", "coverage>=7.0", "mypy>=1.0"]);
    }

    #[test]
    fn dependency_group_cycle_detected() {
        let toml = r#"
[project]
name = "x"

[dependency-groups]
a = [{include-group = "b"}]
b = [{include-group = "a"}]
"#;
        let p = PyProject::from_str(toml).unwrap();
        let err = p.expand_dependency_group("a").unwrap_err();
        assert!(matches!(err, ProjectError::DependencyGroupCycle(_)));
    }

    #[test]
    fn unknown_dependency_group() {
        let toml = r#"
[project]
name = "x"

[dependency-groups]
test = ["pytest"]
"#;
        let p = PyProject::from_str(toml).unwrap();
        let err = p.expand_dependency_group("nope").unwrap_err();
        assert!(matches!(err, ProjectError::UnknownDependencyGroup(_)));
    }

    #[test]
    fn no_project_table_warns_but_parses() {
        let toml = r#"
[build-system]
requires = ["setuptools"]
"#;
        // Should parse successfully, just warn
        let p = PyProject::from_str(toml).unwrap();
        assert!(p.project.is_none());
    }

    #[test]
    fn empty_pyproject() {
        let toml = "";
        let p = PyProject::from_str(toml).unwrap();
        assert!(p.project.is_none());
        assert!(p.build_system.is_none());
    }

    #[test]
    fn parse_fixture_flask() {
        let content = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/flask.toml"
        ))
        .unwrap();
        let p = PyProject::from_str(&content).unwrap();
        let proj = p.project.unwrap();
        assert_eq!(proj.name, "Flask");
        assert!(proj.dependencies.unwrap().len() > 0);
    }

    #[test]
    fn parse_fixture_ruff() {
        let content = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/ruff.toml"
        ))
        .unwrap();
        let p = PyProject::from_str(&content).unwrap();
        let proj = p.project.unwrap();
        assert_eq!(proj.name, "ruff");
    }

    #[test]
    fn transitive_dependency_group_expansion() {
        let toml = r#"
[project]
name = "x"

[dependency-groups]
base = ["numpy>=1.0"]
test = [{include-group = "base"}, "pytest>=7.0"]
dev = [{include-group = "test"}, "mypy>=1.0"]
"#;
        let p = PyProject::from_str(toml).unwrap();
        let expanded = p.expand_dependency_group("dev").unwrap();
        assert_eq!(expanded, vec!["numpy>=1.0", "pytest>=7.0", "mypy>=1.0"]);
    }

    #[test]
    fn diamond_dependency_group_not_false_positive() {
        // Diamond: both B and C include D. This should NOT trigger cycle detection.
        let toml = r#"
[project]
name = "x"

[dependency-groups]
d = ["shared-dep>=1.0"]
b = [{include-group = "d"}, "b-dep>=1.0"]
c = [{include-group = "d"}, "c-dep>=1.0"]
a = [{include-group = "b"}, {include-group = "c"}]
"#;
        let p = PyProject::from_str(toml).unwrap();
        let expanded = p.expand_dependency_group("a").unwrap();
        assert!(expanded.contains(&"shared-dep>=1.0".to_string()));
        assert!(expanded.contains(&"b-dep>=1.0".to_string()));
        assert!(expanded.contains(&"c-dep>=1.0".to_string()));
    }

    #[test]
    fn diamond_dependency_group_no_duplicates() {
        // Diamond: both B and C include D. shared-dep should appear only once.
        let toml = r#"
[project]
name = "x"

[dependency-groups]
d = ["shared-dep>=1.0"]
b = [{include-group = "d"}, "b-dep>=1.0"]
c = [{include-group = "d"}, "c-dep>=1.0"]
a = [{include-group = "b"}, {include-group = "c"}]
"#;
        let p = PyProject::from_str(toml).unwrap();
        let expanded = p.expand_dependency_group("a").unwrap();
        let shared_count = expanded
            .iter()
            .filter(|s| s.as_str() == "shared-dep>=1.0")
            .count();
        assert_eq!(
            shared_count, 1,
            "shared-dep>=1.0 should appear exactly once, got {} in {:?}",
            shared_count, expanded
        );
    }

    // ── [tool.uv] tests ────────────────────────────────────────────

    #[test]
    fn parse_tool_uv_with_indexes_and_sources() {
        let toml = r#"
[project]
name = "myapp"
version = "1.0.0"
dependencies = ["flask>=3.0"]

[tool.uv]
index-url = "https://corporate.example.com/simple"
extra-index-url = ["https://pypi.org/simple"]
dev-dependencies = ["pytest>=8.0", "ruff>=0.4"]
resolution = "lowest-direct"
prerelease = "if-necessary"
python-version = "3.12"
link-mode = "hardlink"
concurrent-downloads = 8

[tool.uv.sources]
flask = { git = "https://github.com/pallets/flask.git", tag = "3.0.0" }
"#;
        let p = PyProject::from_str(toml).unwrap();
        let uv = p.tool_uv().expect("tool.uv should be present");

        assert_eq!(
            uv.index_url.as_deref(),
            Some("https://corporate.example.com/simple")
        );
        assert_eq!(uv.extra_index_url, vec!["https://pypi.org/simple"]);
        assert_eq!(uv.dev_dependencies, vec!["pytest>=8.0", "ruff>=0.4"]);
        assert_eq!(uv.resolution.as_deref(), Some("lowest-direct"));
        assert_eq!(uv.prerelease.as_deref(), Some("if-necessary"));
        assert_eq!(uv.python_version.as_deref(), Some("3.12"));
        assert_eq!(uv.link_mode.as_deref(), Some("hardlink"));
        assert_eq!(uv.concurrent_downloads, Some(8));
        assert!(uv.sources.contains_key("flask"));
        assert_eq!(
            uv.sources["flask"],
            DependencySource::Git {
                git: "https://github.com/pallets/flask.git".to_string(),
                tag: Some("3.0.0".to_string()),
                branch: None,
                rev: None,
                subdirectory: None,
            }
        );
    }

    #[test]
    fn parse_without_tool_uv_returns_none() {
        let toml = r#"
[project]
name = "simple"
version = "0.1.0"
"#;
        let p = PyProject::from_str(toml).unwrap();
        assert!(p.tool_uv().is_none());
    }

    #[test]
    fn parse_tool_uv_workspace_config() {
        let toml = r#"
[project]
name = "mono"
version = "0.1.0"

[tool.uv.workspace]
members = ["packages/*", "libs/*"]
exclude = ["packages/legacy"]
"#;
        let p = PyProject::from_str(toml).unwrap();
        let uv = p.tool_uv().expect("tool.uv should be present");
        let ws = uv.workspace.as_ref().expect("workspace should be present");

        assert_eq!(ws.members, vec!["packages/*", "libs/*"]);
        assert_eq!(ws.exclude, vec!["packages/legacy"]);
    }

    #[test]
    fn parse_tool_uv_named_indexes() {
        let toml = r#"
[project]
name = "idx"
version = "0.1.0"

[[tool.uv.index]]
name = "pytorch"
url = "https://download.pytorch.org/whl/cpu"
explicit = true

[[tool.uv.index]]
name = "internal"
url = "https://internal.example.com/simple"
default = true
"#;
        let p = PyProject::from_str(toml).unwrap();
        let uv = p.tool_uv().expect("tool.uv should be present");

        assert_eq!(uv.index.len(), 2);

        assert_eq!(uv.index[0].name, "pytorch");
        assert_eq!(uv.index[0].url, "https://download.pytorch.org/whl/cpu");
        assert!(uv.index[0].explicit);
        assert!(!uv.index[0].default);

        assert_eq!(uv.index[1].name, "internal");
        assert_eq!(uv.index[1].url, "https://internal.example.com/simple");
        assert!(!uv.index[1].explicit);
        assert!(uv.index[1].default);
    }

    #[test]
    fn tool_uv_unknown_fields_are_ignored() {
        // Forward compatibility: unknown keys should be silently ignored.
        let toml = r#"
[project]
name = "future"
version = "0.1.0"

[tool.uv]
managed = true
package = false
some-future-field = "should be ignored"
another-new-thing = 42
"#;
        let p = PyProject::from_str(toml).unwrap();
        let uv = p.tool_uv().expect("tool.uv should be present");
        assert_eq!(uv.managed, Some(true));
        assert_eq!(uv.package, Some(false));
    }

    #[test]
    fn parse_tool_uv_managed_and_package_flags() {
        let toml = r#"
[project]
name = "virtual"
version = "0.1.0"

[tool.uv]
managed = false
package = false
"#;
        let p = PyProject::from_str(toml).unwrap();
        let uv = p.tool_uv().expect("tool.uv should be present");
        assert_eq!(uv.managed, Some(false));
        assert_eq!(uv.package, Some(false));
    }

    #[test]
    fn parse_tool_uv_constraint_and_override_deps() {
        let toml = r#"
[project]
name = "pinned"
version = "1.0.0"

[tool.uv]
constraint-dependencies = ["numpy<2.0"]
override-dependencies = ["requests==2.31.0"]
default-groups = ["dev", "test"]
environments = ["sys_platform == 'linux'", "sys_platform == 'darwin'"]
"#;
        let p = PyProject::from_str(toml).unwrap();
        let uv = p.tool_uv().expect("tool.uv should be present");

        assert_eq!(uv.constraint_dependencies, vec!["numpy<2.0"]);
        assert_eq!(uv.override_dependencies, vec!["requests==2.31.0"]);
        assert_eq!(uv.default_groups, vec!["dev", "test"]);
        assert_eq!(
            uv.environments,
            vec!["sys_platform == 'linux'", "sys_platform == 'darwin'"]
        );
    }

    #[test]
    fn parse_dependency_source_git_with_tag() {
        let toml = r#"
[project]
name = "x"
version = "0.1.0"

[tool.uv.sources]
mypackage = { git = "https://github.com/org/repo", tag = "v1.0" }
"#;
        let p = PyProject::from_str(toml).unwrap();
        let uv = p.tool_uv().expect("tool.uv should be present");
        assert_eq!(
            uv.sources["mypackage"],
            DependencySource::Git {
                git: "https://github.com/org/repo".to_string(),
                tag: Some("v1.0".to_string()),
                branch: None,
                rev: None,
                subdirectory: None,
            }
        );
    }

    #[test]
    fn parse_dependency_source_path() {
        let toml = r#"
[project]
name = "x"
version = "0.1.0"

[tool.uv.sources]
mypackage = { path = "../local-pkg" }
"#;
        let p = PyProject::from_str(toml).unwrap();
        let uv = p.tool_uv().expect("tool.uv should be present");
        assert_eq!(
            uv.sources["mypackage"],
            DependencySource::Path {
                path: "../local-pkg".to_string(),
                editable: false,
            }
        );
    }

    #[test]
    fn parse_dependency_source_path_editable() {
        let toml = r#"
[project]
name = "x"
version = "0.1.0"

[tool.uv.sources]
mypackage = { path = "../local-pkg", editable = true }
"#;
        let p = PyProject::from_str(toml).unwrap();
        let uv = p.tool_uv().expect("tool.uv should be present");
        assert_eq!(
            uv.sources["mypackage"],
            DependencySource::Path {
                path: "../local-pkg".to_string(),
                editable: true,
            }
        );
    }

    #[test]
    fn parse_dependency_source_url() {
        let toml = r#"
[project]
name = "x"
version = "0.1.0"

[tool.uv.sources]
mypackage = { url = "https://example.com/pkg-1.0.tar.gz" }
"#;
        let p = PyProject::from_str(toml).unwrap();
        let uv = p.tool_uv().expect("tool.uv should be present");
        assert_eq!(
            uv.sources["mypackage"],
            DependencySource::Url {
                url: "https://example.com/pkg-1.0.tar.gz".to_string(),
            }
        );
    }

    #[test]
    fn parse_dependency_source_workspace() {
        let toml = r#"
[project]
name = "x"
version = "0.1.0"

[tool.uv.sources]
mypackage = { workspace = true }
"#;
        let p = PyProject::from_str(toml).unwrap();
        let uv = p.tool_uv().expect("tool.uv should be present");
        assert_eq!(
            uv.sources["mypackage"],
            DependencySource::Workspace { workspace: true }
        );
    }

    #[test]
    fn parse_dependency_source_registry() {
        let toml = r#"
[project]
name = "x"
version = "0.1.0"

[tool.uv.sources]
mypackage = { index = "my-private-index" }
"#;
        let p = PyProject::from_str(toml).unwrap();
        let uv = p.tool_uv().expect("tool.uv should be present");
        assert_eq!(
            uv.sources["mypackage"],
            DependencySource::Registry {
                index: "my-private-index".to_string(),
            }
        );
    }

    #[test]
    fn empty_tool_uv_section_uses_defaults() {
        let toml = r#"
[project]
name = "bare"
version = "0.1.0"

[tool.uv]
"#;
        let p = PyProject::from_str(toml).unwrap();
        let uv = p.tool_uv().expect("tool.uv should be present");
        assert!(uv.managed.is_none());
        assert!(uv.package.is_none());
        assert!(uv.index_url.is_none());
        assert!(uv.extra_index_url.is_empty());
        assert!(uv.dev_dependencies.is_empty());
        assert!(uv.sources.is_empty());
        assert!(uv.index.is_empty());
        assert!(uv.workspace.is_none());
    }

    // ── PEP 621 extended fields tests ──────────────────────────────

    #[test]
    fn parse_full_pep621_fields() {
        let toml = r#"
[project]
name = "myproject"
version = "1.0.0"
description = "A test project"
readme = "README.md"
license = "MIT"
keywords = ["test", "example"]
classifiers = ["Development Status :: 3 - Alpha"]
requires-python = ">=3.8"

[[project.authors]]
name = "Test Author"
email = "test@example.com"

[[project.maintainers]]
name = "Test Maintainer"

[project.urls]
Homepage = "https://example.com"
Repository = "https://github.com/example/project"

[project.entry-points.mygroup]
mycommand = "mypackage:main"
"#;
        let p = PyProject::from_str(toml).unwrap();
        let proj = p.project.as_ref().unwrap();

        assert_eq!(proj.name, "myproject");
        assert_eq!(proj.version.as_deref(), Some("1.0.0"));
        assert_eq!(proj.description.as_deref(), Some("A test project"));
        assert_eq!(proj.requires_python.as_deref(), Some(">=3.8"));

        // readme as string path
        assert_eq!(
            proj.readme,
            Some(ReadmeField::Path("README.md".to_string()))
        );

        // license as SPDX string
        assert_eq!(proj.license, Some(LicenseField::Spdx("MIT".to_string())));

        // keywords
        assert_eq!(proj.keywords.as_ref().unwrap(), &["test", "example"]);

        // classifiers
        assert_eq!(
            proj.classifiers.as_ref().unwrap(),
            &["Development Status :: 3 - Alpha"]
        );

        // authors
        let authors = proj.authors.as_ref().unwrap();
        assert_eq!(authors.len(), 1);
        assert_eq!(authors[0].name.as_deref(), Some("Test Author"));
        assert_eq!(authors[0].email.as_deref(), Some("test@example.com"));

        // maintainers
        let maintainers = proj.maintainers.as_ref().unwrap();
        assert_eq!(maintainers.len(), 1);
        assert_eq!(maintainers[0].name.as_deref(), Some("Test Maintainer"));
        assert!(maintainers[0].email.is_none());

        // urls
        let urls = proj.urls.as_ref().unwrap();
        assert_eq!(urls["Homepage"], "https://example.com");
        assert_eq!(urls["Repository"], "https://github.com/example/project");

        // entry-points
        let ep = proj.entry_points.as_ref().unwrap();
        assert_eq!(ep["mygroup"]["mycommand"], "mypackage:main");
    }

    #[test]
    fn parse_readme_as_table() {
        let toml = r#"
[project]
name = "myproject"
readme = {file = "README.rst", content-type = "text/x-rst"}
"#;
        let p = PyProject::from_str(toml).unwrap();
        let proj = p.project.as_ref().unwrap();

        match &proj.readme {
            Some(ReadmeField::Table {
                file,
                text,
                content_type,
            }) => {
                assert_eq!(file.as_deref(), Some("README.rst"));
                assert!(text.is_none());
                assert_eq!(content_type.as_deref(), Some("text/x-rst"));
            }
            other => panic!("expected ReadmeField::Table, got {:?}", other),
        }
    }

    #[test]
    fn parse_license_as_table_with_file() {
        let toml = r#"
[project]
name = "myproject"
license = {file = "LICENSE"}
"#;
        let p = PyProject::from_str(toml).unwrap();
        let proj = p.project.as_ref().unwrap();

        match &proj.license {
            Some(LicenseField::Table { text, file }) => {
                assert_eq!(file.as_deref(), Some("LICENSE"));
                assert!(text.is_none());
            }
            other => panic!("expected LicenseField::Table, got {:?}", other),
        }
    }

    #[test]
    fn parse_license_as_table_with_text() {
        let toml = r#"
[project]
name = "myproject"

[project.license]
text = "MIT License\n\nCopyright (c) 2024"
"#;
        let p = PyProject::from_str(toml).unwrap();
        let proj = p.project.as_ref().unwrap();

        match &proj.license {
            Some(LicenseField::Table { text, file }) => {
                assert!(text.is_some());
                assert!(file.is_none());
            }
            other => panic!("expected LicenseField::Table, got {:?}", other),
        }
    }

    #[test]
    fn new_fields_default_to_none() {
        // Existing minimal TOML should still parse with new fields as None.
        let toml = r#"
[project]
name = "tiny"
"#;
        let p = PyProject::from_str(toml).unwrap();
        let proj = p.project.unwrap();
        assert_eq!(proj.name, "tiny");
        assert!(proj.readme.is_none());
        assert!(proj.license.is_none());
        assert!(proj.license_files.is_none());
        assert!(proj.authors.is_none());
        assert!(proj.maintainers.is_none());
        assert!(proj.keywords.is_none());
        assert!(proj.classifiers.is_none());
        assert!(proj.urls.is_none());
        assert!(proj.entry_points.is_none());
    }

    #[test]
    fn parse_license_files() {
        let toml = r#"
[project]
name = "myproject"
license-files = ["LICENSE*", "NOTICE"]
"#;
        let p = PyProject::from_str(toml).unwrap();
        let proj = p.project.as_ref().unwrap();
        assert_eq!(
            proj.license_files.as_ref().unwrap(),
            &["LICENSE*", "NOTICE"]
        );
    }

    #[test]
    fn parse_multiple_entry_point_groups() {
        let toml = r#"
[project]
name = "myproject"

[project.entry-points.console]
mycli = "mypackage.cli:main"

[project.entry-points.gui]
mygui = "mypackage.gui:run"
"#;
        let p = PyProject::from_str(toml).unwrap();
        let proj = p.project.as_ref().unwrap();
        let ep = proj.entry_points.as_ref().unwrap();
        assert_eq!(ep.len(), 2);
        assert_eq!(ep["console"]["mycli"], "mypackage.cli:main");
        assert_eq!(ep["gui"]["mygui"], "mypackage.gui:run");
    }

    // ── Improvement 3: TOML parse errors include line/column ─────

    #[test]
    fn toml_parse_error_includes_line_info() {
        // Intentionally broken TOML: missing closing quote on line 3
        let broken = r#"
[project]
name = "unclosed
version = "1.0"
"#;
        let err = PyProject::from_str(broken).unwrap_err();
        let msg = err.to_string();
        // The error message should mention line information
        assert!(
            msg.contains("line") || msg.contains("byte offset"),
            "TOML parse error should include position info: {msg}"
        );
        assert!(
            msg.contains("pyproject.toml"),
            "error should reference pyproject.toml: {msg}"
        );
    }

    #[test]
    fn toml_parse_error_wrong_type_includes_key_and_line() {
        // `name` should be a string, not an integer
        let broken = r#"
[project]
name = 42
"#;
        let err = PyProject::from_str(broken).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("line") || msg.contains("byte offset"),
            "type mismatch error should include position info: {msg}"
        );
        assert!(
            msg.contains("pyproject.toml"),
            "error should reference pyproject.toml: {msg}"
        );
    }
}
