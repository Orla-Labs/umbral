use std::path::PathBuf;

use clap::Parser;
use miette::{Context, IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use toml_edit::DocumentMut;

use super::normalize_package_name;

#[derive(Debug, Parser)]
pub struct RemoveArgs {
    /// Packages to remove (matched by PEP 503 normalized name)
    #[arg(required = true)]
    packages: Vec<String>,

    /// Remove from a development dependency group (shorthand for --group dev)
    #[arg(long)]
    dev: bool,

    /// Remove from a specific dependency group
    #[arg(long)]
    group: Option<String>,

    /// Path to pyproject.toml
    #[arg(long, default_value = "./pyproject.toml")]
    project: PathBuf,
}

pub fn cmd_remove(args: RemoveArgs) -> Result<()> {
    let group = resolve_group(args.dev, args.group.as_deref());

    let content = std::fs::read_to_string(&args.project)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read {}", args.project.display()))?;

    let mut doc: DocumentMut = content
        .parse()
        .into_diagnostic()
        .wrap_err("failed to parse pyproject.toml")?;

    let removed = remove_packages_from_doc(&mut doc, &args.packages, group.as_deref())?;

    if removed.is_empty() {
        eprintln!(
            "{} No matching dependencies found to remove.",
            "⚠".yellow().bold()
        );
        return Ok(());
    }

    std::fs::write(&args.project, doc.to_string())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to write {}", args.project.display()))?;

    for pkg in &removed {
        eprintln!("  {} {}", "-".red().bold(), pkg.cyan());
    }

    // Auto-resolve + sync: lock, create venv if needed, remove stale packages.
    let project_dir = args
        .project
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let lockfile_path = project_dir.join("uv.lock");
    let venv_path = project_dir.join(".venv");

    super::ensure_synced(&args.project, &lockfile_path, &venv_path, None)?;

    Ok(())
}

/// Determine the target group from the `--dev` and `--group` flags.
fn resolve_group(dev: bool, group: Option<&str>) -> Option<String> {
    if let Some(g) = group {
        Some(g.to_string())
    } else if dev {
        Some("dev".to_string())
    } else {
        None
    }
}

/// Remove packages from a TOML document, returning the original dep strings
/// that were removed.
///
/// Matches by PEP 503 normalized package name so that `remove Flask` matches
/// `flask>=2.0` in the dependency list.
pub fn remove_packages_from_doc(
    doc: &mut DocumentMut,
    packages: &[String],
    group: Option<&str>,
) -> Result<Vec<String>> {
    let targets: Vec<String> = packages.iter().map(|p| normalize_package_name(p)).collect();

    let removed = match group {
        None => {
            let project = doc
                .get_mut("project")
                .and_then(|p| p.as_table_mut())
                .ok_or_else(|| miette::miette!("[project] table not found in pyproject.toml"))?;

            let deps = project
                .get_mut("dependencies")
                .and_then(|d| d.as_array_mut())
                .ok_or_else(|| miette::miette!("[project].dependencies is not an array"))?;

            remove_from_array(deps, &targets)
        }
        Some(group_name) => {
            let groups = doc
                .get_mut("dependency-groups")
                .and_then(|g| g.as_table_mut())
                .ok_or_else(|| {
                    miette::miette!("[dependency-groups] table not found in pyproject.toml")
                })?;

            let group_array = groups
                .get_mut(group_name)
                .and_then(|g| g.as_array_mut())
                .ok_or_else(|| {
                    miette::miette!("[dependency-groups.{}] is not an array", group_name)
                })?;

            remove_from_array(group_array, &targets)
        }
    };

    Ok(removed)
}

/// Remove matching entries from a `toml_edit::Array`, returning the original
/// string values that were removed.
fn remove_from_array(array: &mut toml_edit::Array, targets: &[String]) -> Vec<String> {
    let mut removed = Vec::new();

    // Collect indices to remove (in reverse order so removal doesn't shift later indices).
    let mut indices_to_remove: Vec<usize> = Vec::new();

    for (i, item) in array.iter().enumerate() {
        if let Some(s) = item.as_str() {
            // Try to parse as PEP 508 to extract the name; fall back to
            // treating the whole string as a name.
            let dep_name = if let Ok(req) = umbral_pep508::Requirement::parse(s) {
                normalize_package_name(req.name.as_str())
            } else {
                normalize_package_name(s)
            };

            if targets.contains(&dep_name) {
                indices_to_remove.push(i);
                removed.push(s.to_string());
            }
        }
    }

    // Remove from the end so indices stay valid.
    for i in indices_to_remove.into_iter().rev() {
        array.remove(i);
    }

    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_remove_by_normalized_name() {
        let content = r#"[project]
name = "test-pkg"
version = "0.1.0"
dependencies = [
    "requests>=2.28",
    "Flask>=2.0",
    "click",
]
"#;
        let mut doc: DocumentMut = content.parse().unwrap();

        let removed =
            remove_packages_from_doc(&mut doc, &["flask".into(), "Requests".into()], None).unwrap();

        // Should match Flask>=2.0 via normalized "flask" and requests>=2.28 via "requests".
        assert_eq!(removed.len(), 2);
        assert!(removed.contains(&"Flask>=2.0".to_string()));
        assert!(removed.contains(&"requests>=2.28".to_string()));

        // Only "click" should remain.
        let deps = doc["project"]["dependencies"].as_array().unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps.get(0).unwrap().as_str().unwrap(), "click");
    }

    #[test]
    fn test_remove_from_group() {
        let content = r#"[project]
name = "test-pkg"
version = "0.1.0"
dependencies = []

[dependency-groups]
test = ["pytest", "pytest-cov>=4.0"]
"#;
        let mut doc: DocumentMut = content.parse().unwrap();

        let removed =
            remove_packages_from_doc(&mut doc, &["pytest-cov".into()], Some("test")).unwrap();

        assert_eq!(removed, vec!["pytest-cov>=4.0"]);

        let group = doc["dependency-groups"]["test"].as_array().unwrap();
        assert_eq!(group.len(), 1);
        assert_eq!(group.get(0).unwrap().as_str().unwrap(), "pytest");
    }

    #[test]
    fn test_remove_preserves_formatting() {
        let content = r#"# Project
[project]
name = "app"
version = "1.0.0"  # stable release
dependencies = [
    "requests>=2.28",
    "click",
]
"#;
        let mut doc: DocumentMut = content.parse().unwrap();

        remove_packages_from_doc(&mut doc, &["requests".into()], None).unwrap();

        let output = doc.to_string();
        assert!(output.contains("# Project"));
        assert!(output.contains("# stable release"));
        assert!(!output.contains("requests"));
        assert!(output.contains("click"));
    }

    #[test]
    fn test_remove_nonexistent_returns_empty() {
        let content = r#"[project]
name = "test-pkg"
version = "0.1.0"
dependencies = ["requests"]
"#;
        let mut doc: DocumentMut = content.parse().unwrap();

        let removed = remove_packages_from_doc(&mut doc, &["flask".into()], None).unwrap();

        assert!(removed.is_empty());
        // Original deps unchanged.
        let deps = doc["project"]["dependencies"].as_array().unwrap();
        assert_eq!(deps.len(), 1);
    }
}
