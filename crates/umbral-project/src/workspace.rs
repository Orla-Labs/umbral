//! Workspace discovery and management for monorepo support.
//!
//! A workspace is defined by a root `pyproject.toml` that contains a
//! `[tool.uv.workspace]` section with `members` (and optional `exclude`)
//! glob patterns. This module walks up the filesystem from a given
//! starting directory, finds the workspace root, and discovers all
//! member packages.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::{DependencySource, PyProject};

// ── Errors ──────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("failed to read {path}: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("failed to parse {path}: {source}")]
    ParseProject {
        path: PathBuf,
        source: Box<crate::ProjectError>,
    },

    #[error("workspace member pattern \"{pattern}\" matched no directories")]
    NoMembers { pattern: String },

    #[error("workspace member directory {path} has no pyproject.toml")]
    MissingPyproject { path: PathBuf },

    #[error("glob pattern error: {0}")]
    Glob(String),
}

// ── Types ───────────────────────────────────────────────────────────

/// Represents a discovered workspace.
#[derive(Debug)]
pub struct Workspace {
    /// Path to the workspace root (directory containing the root pyproject.toml).
    pub root: PathBuf,
    /// The root project's parsed pyproject.toml.
    pub root_project: PyProject,
    /// Discovered member packages (path, parsed pyproject).
    pub members: Vec<WorkspaceMember>,
}

/// A single member within a workspace.
#[derive(Debug)]
pub struct WorkspaceMember {
    /// Path to the member's directory.
    pub path: PathBuf,
    /// The member's parsed pyproject.toml.
    pub project: PyProject,
}

// ── Implementation ──────────────────────────────────────────────────

impl Workspace {
    /// Discover a workspace by walking up from `start_dir` looking for a
    /// `pyproject.toml` with `[tool.uv.workspace]` configuration.
    ///
    /// Returns `Ok(None)` if no workspace root is found (i.e. we reach
    /// the filesystem root without finding a workspace-enabled pyproject).
    pub fn discover(start_dir: &Path) -> Result<Option<Workspace>, WorkspaceError> {
        let start = start_dir
            .canonicalize()
            .map_err(|e| WorkspaceError::ReadFile {
                path: start_dir.to_path_buf(),
                source: e,
            })?;

        let mut current = start.as_path();
        loop {
            let pyproject_path = current.join("pyproject.toml");
            if pyproject_path.is_file() {
                let content = std::fs::read_to_string(&pyproject_path).map_err(|e| {
                    WorkspaceError::ReadFile {
                        path: pyproject_path.clone(),
                        source: e,
                    }
                })?;

                let pyproject =
                    PyProject::from_str(&content).map_err(|e| WorkspaceError::ParseProject {
                        path: pyproject_path.clone(),
                        source: Box::new(e),
                    })?;

                // Check for [tool.uv.workspace]
                if let Some(ws_config) = pyproject
                    .tool
                    .as_ref()
                    .and_then(|t| t.uv.as_ref())
                    .and_then(|uv| uv.workspace.as_ref())
                {
                    let members =
                        Self::discover_members(current, &ws_config.members, &ws_config.exclude)?;

                    return Ok(Some(Workspace {
                        root: current.to_path_buf(),
                        root_project: pyproject,
                        members,
                    }));
                }
            }

            // Walk up one level.
            match current.parent() {
                Some(parent) if parent != current => {
                    current = parent;
                }
                _ => break,
            }
        }

        Ok(None)
    }

    /// Expand glob patterns from `[tool.uv.workspace].members` relative to
    /// the workspace root, filtering out directories that match any `exclude`
    /// pattern.
    fn discover_members(
        root: &Path,
        members: &[String],
        exclude: &[String],
    ) -> Result<Vec<WorkspaceMember>, WorkspaceError> {
        let mut discovered: Vec<WorkspaceMember> = Vec::new();
        let mut seen_paths: HashSet<PathBuf> = HashSet::new();

        // Pre-compile exclude patterns.
        let exclude_patterns: Vec<glob::Pattern> = exclude
            .iter()
            .map(|pat| {
                glob::Pattern::new(pat).map_err(|e| WorkspaceError::Glob(format!("{}: {}", pat, e)))
            })
            .collect::<Result<Vec<_>, _>>()?;

        for pattern in members {
            let glob_path = root.join(pattern);
            let glob_str = glob_path.to_string_lossy().to_string();

            let entries: Vec<PathBuf> = glob::glob(&glob_str)
                .map_err(|e| WorkspaceError::Glob(format!("{}: {}", pattern, e)))?
                .filter_map(|entry| entry.ok())
                .filter(|path| path.is_dir())
                .collect();

            if entries.is_empty() {
                return Err(WorkspaceError::NoMembers {
                    pattern: pattern.clone(),
                });
            }

            for dir in entries {
                let canonical = dir.canonicalize().map_err(|e| WorkspaceError::ReadFile {
                    path: dir.clone(),
                    source: e,
                })?;

                // Skip if we've already seen this path (dedup across patterns).
                if !seen_paths.insert(canonical.clone()) {
                    continue;
                }

                // Check exclude patterns against the relative path from root.
                let relative = canonical.strip_prefix(root).unwrap_or(&canonical);
                let relative_str = relative.to_string_lossy();

                let excluded = exclude_patterns
                    .iter()
                    .any(|pat| pat.matches(&relative_str));
                if excluded {
                    continue;
                }

                // Parse the member's pyproject.toml.
                let member_pyproject_path = canonical.join("pyproject.toml");
                if !member_pyproject_path.is_file() {
                    return Err(WorkspaceError::MissingPyproject { path: canonical });
                }

                let content = std::fs::read_to_string(&member_pyproject_path).map_err(|e| {
                    WorkspaceError::ReadFile {
                        path: member_pyproject_path.clone(),
                        source: e,
                    }
                })?;

                let project =
                    PyProject::from_str(&content).map_err(|e| WorkspaceError::ParseProject {
                        path: member_pyproject_path,
                        source: Box::new(e),
                    })?;

                discovered.push(WorkspaceMember {
                    path: canonical,
                    project,
                });
            }
        }

        Ok(discovered)
    }

    /// Collect all dependencies across workspace members for unified resolution.
    ///
    /// Workspace member references (sources with `workspace = true`) are excluded
    /// since they are local packages, not external PyPI dependencies.
    /// Dependencies are deduplicated by their PEP 503-normalized name.
    pub fn all_dependencies(&self) -> Vec<String> {
        let workspace_sources = self.workspace_source_names();
        let mut seen = HashSet::new();
        let mut deps = Vec::new();

        let collect = |pyproject: &PyProject,
                       deps: &mut Vec<String>,
                       seen: &mut HashSet<String>,
                       ws_sources: &HashSet<String>| {
            for dep in pyproject.all_dependencies() {
                // Extract the package name (before any version specifier).
                let name = dep
                    .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_' && c != '.')
                    .next()
                    .unwrap_or(&dep);
                let normalized = normalize_name(name);

                if ws_sources.contains(&normalized) {
                    continue;
                }
                if seen.insert(normalized) {
                    deps.push(dep);
                }
            }
        };

        collect(&self.root_project, &mut deps, &mut seen, &workspace_sources);

        for member in &self.members {
            collect(&member.project, &mut deps, &mut seen, &workspace_sources);
        }

        deps
    }

    /// Check if a package name is a workspace member (root or any member).
    pub fn is_member(&self, name: &str) -> bool {
        let normalized = normalize_name(name);

        // Check root project.
        if let Some(proj) = self.root_project.project.as_ref() {
            if normalize_name(&proj.name) == normalized {
                return true;
            }
        }

        // Check members.
        for member in &self.members {
            if let Some(proj) = member.project.project.as_ref() {
                if normalize_name(&proj.name) == normalized {
                    return true;
                }
            }
        }

        false
    }

    /// Return the set of normalized names that are workspace-source dependencies
    /// (i.e., have `{ workspace = true }` in `[tool.uv.sources]`).
    fn workspace_source_names(&self) -> HashSet<String> {
        let mut names = HashSet::new();
        Self::collect_ws_sources(&self.root_project, &mut names);
        for member in &self.members {
            Self::collect_ws_sources(&member.project, &mut names);
        }
        names
    }

    fn collect_ws_sources(pyproject: &PyProject, names: &mut HashSet<String>) {
        if let Some(uv) = pyproject.tool.as_ref().and_then(|t| t.uv.as_ref()) {
            for (name, source) in &uv.sources {
                if matches!(source, DependencySource::Workspace { workspace: true }) {
                    names.insert(normalize_name(name));
                }
            }
        }
    }

    /// Find the member whose directory contains `dir` (or equals it).
    /// Returns the member's `WorkspaceMember` if found.
    pub fn member_for_dir(&self, dir: &Path) -> Option<&WorkspaceMember> {
        let canonical = dir.canonicalize().ok()?;
        self.members.iter().find(|m| canonical.starts_with(&m.path))
    }
}

/// Normalize a package name per PEP 503 (lowercase, replace separators with `-`,
/// collapse consecutive separators).
fn normalize_name(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    let mut prev_sep = false;
    for ch in name.chars() {
        match ch {
            '-' | '_' | '.' => {
                if !prev_sep {
                    result.push('-');
                }
                prev_sep = true;
            }
            c => {
                result.push(c.to_ascii_lowercase());
                prev_sep = false;
            }
        }
    }
    result
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Helper: create a directory and write a pyproject.toml into it.
    fn write_pyproject(dir: &Path, content: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("pyproject.toml"), content).unwrap();
    }

    /// Build a standard workspace fixture:
    ///
    /// ```text
    /// root/
    ///   pyproject.toml          # [tool.uv.workspace] members = ["packages/*"]
    ///   packages/
    ///     pkg-a/
    ///       pyproject.toml      # regular package
    ///     pkg-b/
    ///       pyproject.toml      # depends on pkg-a with workspace = true
    /// ```
    fn make_workspace(tmp: &TempDir) -> PathBuf {
        let root = tmp.path().to_path_buf();

        write_pyproject(
            &root,
            r#"
[project]
name = "my-monorepo"
version = "0.1.0"
dependencies = ["requests>=2.28"]

[tool.uv.workspace]
members = ["packages/*"]
"#,
        );

        write_pyproject(
            &root.join("packages").join("pkg-a"),
            r#"
[project]
name = "pkg-a"
version = "0.1.0"
dependencies = ["click>=8.0"]
"#,
        );

        write_pyproject(
            &root.join("packages").join("pkg-b"),
            r#"
[project]
name = "pkg-b"
version = "0.1.0"
dependencies = ["pkg-a", "flask>=3.0"]

[tool.uv.sources]
pkg-a = { workspace = true }
"#,
        );

        root
    }

    #[test]
    fn test_discover_workspace() {
        let tmp = TempDir::new().unwrap();
        let root = make_workspace(&tmp);

        // Discover from a member directory.
        let ws = Workspace::discover(&root.join("packages").join("pkg-a"))
            .unwrap()
            .expect("should discover workspace");

        assert_eq!(
            ws.root.canonicalize().unwrap(),
            root.canonicalize().unwrap()
        );
        assert_eq!(ws.members.len(), 2);

        let member_names: Vec<String> = ws
            .members
            .iter()
            .filter_map(|m| m.project.project.as_ref().map(|p| p.name.clone()))
            .collect();
        assert!(member_names.contains(&"pkg-a".to_string()));
        assert!(member_names.contains(&"pkg-b".to_string()));
    }

    #[test]
    fn test_discover_no_workspace() {
        let tmp = TempDir::new().unwrap();
        write_pyproject(
            tmp.path(),
            r#"
[project]
name = "standalone"
version = "0.1.0"
dependencies = ["requests"]
"#,
        );

        let result = Workspace::discover(tmp.path()).unwrap();
        assert!(
            result.is_none(),
            "standalone project should not be a workspace"
        );
    }

    #[test]
    fn test_workspace_member_globs() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_pyproject(
            root,
            r#"
[project]
name = "monorepo"
version = "0.1.0"

[tool.uv.workspace]
members = ["packages/*"]
"#,
        );

        // Create 3 member packages.
        for name in &["alpha", "beta", "gamma"] {
            write_pyproject(
                &root.join("packages").join(name),
                &format!(
                    r#"
[project]
name = "{name}"
version = "0.1.0"
"#,
                ),
            );
        }

        let ws = Workspace::discover(root)
            .unwrap()
            .expect("should discover workspace");

        assert_eq!(ws.members.len(), 3);
    }

    #[test]
    fn test_workspace_exclude() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_pyproject(
            root,
            r#"
[project]
name = "monorepo"
version = "0.1.0"

[tool.uv.workspace]
members = ["packages/*"]
exclude = ["packages/legacy"]
"#,
        );

        for name in &["good", "legacy"] {
            write_pyproject(
                &root.join("packages").join(name),
                &format!(
                    r#"
[project]
name = "{name}"
version = "0.1.0"
"#,
                ),
            );
        }

        let ws = Workspace::discover(root)
            .unwrap()
            .expect("should discover workspace");

        assert_eq!(ws.members.len(), 1);
        let member_name = ws.members[0]
            .project
            .project
            .as_ref()
            .unwrap()
            .name
            .as_str();
        assert_eq!(member_name, "good");
    }

    #[test]
    fn test_workspace_all_dependencies() {
        let tmp = TempDir::new().unwrap();
        let root = make_workspace(&tmp);

        let ws = Workspace::discover(&root)
            .unwrap()
            .expect("should discover workspace");

        let deps = ws.all_dependencies();

        // "requests", "click", "flask" should be present.
        // "pkg-a" should NOT be present (workspace source).
        let dep_text = deps.join(" ");
        assert!(dep_text.contains("requests"), "should contain requests");
        assert!(dep_text.contains("click"), "should contain click");
        assert!(dep_text.contains("flask"), "should contain flask");

        // pkg-a is a workspace source — should be excluded.
        let has_pkg_a = deps.iter().any(|d| {
            let name = d
                .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_' && c != '.')
                .next()
                .unwrap_or(d);
            normalize_name(name) == "pkg-a"
        });
        assert!(!has_pkg_a, "workspace source pkg-a should be excluded");

        // Deduplication: no repeated normalized names.
        let mut seen = HashSet::new();
        for dep in &deps {
            let name = dep
                .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_' && c != '.')
                .next()
                .unwrap_or(dep);
            let n = normalize_name(name);
            assert!(seen.insert(n.clone()), "duplicate dependency: {}", n);
        }
    }

    #[test]
    fn test_is_member() {
        let tmp = TempDir::new().unwrap();
        let root = make_workspace(&tmp);

        let ws = Workspace::discover(&root)
            .unwrap()
            .expect("should discover workspace");

        assert!(ws.is_member("pkg-a"));
        assert!(ws.is_member("pkg-b"));
        assert!(ws.is_member("my-monorepo"));
        assert!(ws.is_member("Pkg-A")); // case-insensitive
        assert!(ws.is_member("pkg_a")); // underscore normalization
        assert!(!ws.is_member("nonexistent"));
    }

    #[test]
    fn test_discover_from_root() {
        let tmp = TempDir::new().unwrap();
        let root = make_workspace(&tmp);

        // Discover from the root directory itself.
        let ws = Workspace::discover(&root)
            .unwrap()
            .expect("should discover workspace from root");

        assert_eq!(
            ws.root.canonicalize().unwrap(),
            root.canonicalize().unwrap()
        );
        assert_eq!(ws.members.len(), 2);
    }

    #[test]
    fn test_member_for_dir() {
        let tmp = TempDir::new().unwrap();
        let root = make_workspace(&tmp);

        let ws = Workspace::discover(&root)
            .unwrap()
            .expect("should discover workspace");

        let member_a = ws.member_for_dir(&root.join("packages").join("pkg-a"));
        assert!(member_a.is_some());
        assert_eq!(
            member_a.unwrap().project.project.as_ref().unwrap().name,
            "pkg-a"
        );

        let member_none = ws.member_for_dir(&root.join("nonexistent"));
        assert!(member_none.is_none());
    }

    #[test]
    fn test_normalize_name() {
        assert_eq!(normalize_name("My_Package"), "my-package");
        assert_eq!(normalize_name("my.package"), "my-package");
        assert_eq!(normalize_name("My_Cool.Package"), "my-cool-package");
        assert_eq!(normalize_name("REQUESTS"), "requests");
        assert_eq!(normalize_name("my--pkg"), "my-pkg");
    }
}
