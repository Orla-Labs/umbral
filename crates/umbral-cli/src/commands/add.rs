use std::path::PathBuf;

use clap::Parser;
use miette::{Context, IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use toml_edit::{Array, DocumentMut, Item, Value};

use super::normalize_package_name;

#[derive(Debug, Parser)]
pub struct AddArgs {
    /// Packages to add (PEP 508 requirement strings, e.g. "requests" or "flask>=2.0")
    #[arg(required = true)]
    packages: Vec<String>,

    /// Add as a development dependency (shorthand for --group dev)
    #[arg(long)]
    dev: bool,

    /// Add to a specific dependency group
    #[arg(long)]
    group: Option<String>,

    /// Path to pyproject.toml
    #[arg(long, default_value = "./pyproject.toml")]
    project: PathBuf,
}

pub fn cmd_add(args: AddArgs) -> Result<()> {
    // Validate all package strings parse as PEP 508 before touching the file.
    for pkg in &args.packages {
        umbral_pep508::Requirement::parse(pkg)
            .map_err(|e| miette::miette!("invalid PEP 508 requirement '{}': {}", pkg, e))?;
    }

    let group = resolve_group(args.dev, args.group.as_deref());

    let content = std::fs::read_to_string(&args.project)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read {}", args.project.display()))?;

    let mut doc: DocumentMut = content
        .parse()
        .into_diagnostic()
        .wrap_err("failed to parse pyproject.toml")?;

    let added = add_packages_to_doc(&mut doc, &args.packages, group.as_deref())?;

    std::fs::write(&args.project, doc.to_string())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to write {}", args.project.display()))?;

    for pkg in &added {
        eprintln!("  {} {}", "+".green().bold(), pkg.cyan());
    }

    // Auto-resolve + sync: lock, create venv if needed, install packages.
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

/// Add packages to a TOML document, returning the list of strings that were added.
///
/// If `group` is `None`, adds to `[project.dependencies]`.
/// If `group` is `Some(name)`, adds to `[dependency-groups.NAME]`.
pub fn add_packages_to_doc(
    doc: &mut DocumentMut,
    packages: &[String],
    group: Option<&str>,
) -> Result<Vec<String>> {
    let mut added = Vec::new();

    match group {
        None => {
            // Ensure [project] table exists.
            if doc.get("project").is_none() {
                return Err(miette::miette!(
                    "[project] table not found in pyproject.toml"
                ));
            }

            let project = doc["project"]
                .as_table_mut()
                .ok_or_else(|| miette::miette!("[project] is not a table"))?;

            // Ensure dependencies array exists.
            if project.get("dependencies").is_none() {
                project.insert("dependencies", Item::Value(Value::Array(Array::new())));
            }

            let deps = project["dependencies"]
                .as_array_mut()
                .ok_or_else(|| miette::miette!("[project].dependencies is not an array"))?;

            for pkg in packages {
                // Check if already present (by normalized name).
                let req = umbral_pep508::Requirement::parse(pkg)
                    .map_err(|e| miette::miette!("invalid requirement '{}': {}", pkg, e))?;
                let norm = normalize_package_name(req.name.as_str());

                let already = deps.iter().any(|item| {
                    if let Some(s) = item.as_str() {
                        if let Ok(existing) = umbral_pep508::Requirement::parse(s) {
                            normalize_package_name(existing.name.as_str()) == norm
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                });

                if !already {
                    deps.push(pkg.as_str());
                    added.push(pkg.clone());
                } else {
                    eprintln!(
                        "  {} {} is already in dependencies (skipped)",
                        "→".dimmed(),
                        norm.dimmed()
                    );
                }
            }
        }
        Some(group_name) => {
            // Ensure [dependency-groups] table exists.
            if doc.get("dependency-groups").is_none() {
                doc.insert("dependency-groups", Item::Table(toml_edit::Table::new()));
            }

            let groups = doc["dependency-groups"]
                .as_table_mut()
                .ok_or_else(|| miette::miette!("[dependency-groups] is not a table"))?;

            // Ensure the specific group array exists.
            if groups.get(group_name).is_none() {
                groups.insert(group_name, Item::Value(Value::Array(Array::new())));
            }

            let group_array = groups[group_name].as_array_mut().ok_or_else(|| {
                miette::miette!("[dependency-groups.{}] is not an array", group_name)
            })?;

            for pkg in packages {
                let req = umbral_pep508::Requirement::parse(pkg)
                    .map_err(|e| miette::miette!("invalid requirement '{}': {}", pkg, e))?;
                let norm = normalize_package_name(req.name.as_str());

                let already = group_array.iter().any(|item| {
                    if let Some(s) = item.as_str() {
                        if let Ok(existing) = umbral_pep508::Requirement::parse(s) {
                            normalize_package_name(existing.name.as_str()) == norm
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                });

                if !already {
                    group_array.push(pkg.as_str());
                    added.push(pkg.clone());
                } else {
                    eprintln!(
                        "  {} {} is already in group '{}' (skipped)",
                        "→".dimmed(),
                        norm.dimmed(),
                        group_name
                    );
                }
            }
        }
    }

    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_pyproject() -> String {
        r#"[project]
name = "test-pkg"
version = "0.1.0"
description = ""
requires-python = ">=3.12"
dependencies = []
"#
        .to_string()
    }

    #[test]
    fn test_add_appends_to_dependencies() {
        let mut doc: DocumentMut = minimal_pyproject().parse().unwrap();

        let added =
            add_packages_to_doc(&mut doc, &["requests".into(), "flask>=2.0".into()], None).unwrap();

        assert_eq!(added, vec!["requests", "flask>=2.0"]);

        let deps = doc["project"]["dependencies"].as_array().unwrap();
        assert_eq!(deps.len(), 2);
        assert_eq!(deps.get(0).unwrap().as_str().unwrap(), "requests");
        assert_eq!(deps.get(1).unwrap().as_str().unwrap(), "flask>=2.0");
    }

    #[test]
    fn test_add_skips_duplicate_by_normalized_name() {
        let content = r#"[project]
name = "test-pkg"
version = "0.1.0"
dependencies = ["requests>=1.0"]
"#;
        let mut doc: DocumentMut = content.parse().unwrap();

        let added = add_packages_to_doc(&mut doc, &["Requests>=2.0".into()], None).unwrap();

        // Should skip because normalized "requests" already exists.
        assert!(added.is_empty());
        let deps = doc["project"]["dependencies"].as_array().unwrap();
        assert_eq!(deps.len(), 1);
    }

    #[test]
    fn test_add_to_group() {
        let mut doc: DocumentMut = minimal_pyproject().parse().unwrap();

        let added = add_packages_to_doc(
            &mut doc,
            &["pytest".into(), "pytest-cov>=4.0".into()],
            Some("test"),
        )
        .unwrap();

        assert_eq!(added, vec!["pytest", "pytest-cov>=4.0"]);

        let group = doc["dependency-groups"]["test"].as_array().unwrap();
        assert_eq!(group.len(), 2);
        assert_eq!(group.get(0).unwrap().as_str().unwrap(), "pytest");
    }

    #[test]
    fn test_add_preserves_formatting() {
        let content = r#"# My project config
[project]
name = "my-app"
version = "1.0.0"  # stable
description = "A cool app"
requires-python = ">=3.10"
dependencies = [
    "requests>=2.28",  # HTTP client
]
"#;
        let mut doc: DocumentMut = content.parse().unwrap();

        add_packages_to_doc(&mut doc, &["flask>=2.0".into()], None).unwrap();

        let output = doc.to_string();
        // Comments should be preserved.
        assert!(output.contains("# My project config"));
        assert!(output.contains("# stable"));
        assert!(output.contains("# HTTP client"));
        // New dep should be present.
        assert!(output.contains("flask>=2.0"));
    }

    #[test]
    fn test_add_creates_dependencies_array_if_missing() {
        let content = r#"[project]
name = "bare"
version = "0.1.0"
"#;
        let mut doc: DocumentMut = content.parse().unwrap();

        let added = add_packages_to_doc(&mut doc, &["click".into()], None).unwrap();

        assert_eq!(added, vec!["click"]);
        let deps = doc["project"]["dependencies"].as_array().unwrap();
        assert_eq!(deps.len(), 1);
    }
}
