use std::path::PathBuf;

use clap::Parser;
use miette::{Context, IntoDiagnostic, Result};
use owo_colors::OwoColorize;

#[derive(Debug, Parser)]
pub struct InitArgs {
    /// Project name (defaults to current directory name)
    #[arg(long)]
    name: Option<String>,

    /// Python version specifier (e.g. ">=3.10")
    #[arg(long, default_value = None)]
    python: Option<String>,

    /// Directory in which to create pyproject.toml
    #[arg(long, default_value = ".")]
    dir: PathBuf,
}

pub fn cmd_init(args: InitArgs) -> Result<()> {
    let dir = if args.dir.is_absolute() {
        args.dir.clone()
    } else {
        std::env::current_dir()
            .into_diagnostic()
            .wrap_err("failed to determine current directory")?
            .join(&args.dir)
    };

    let pyproject_path = dir.join("pyproject.toml");

    if pyproject_path.exists() {
        return Err(miette::miette!("pyproject.toml already exists"));
    }

    let project_name = match &args.name {
        Some(n) => sanitize_package_name(n),
        None => {
            let dir_name = dir
                .file_name()
                .and_then(|n: &std::ffi::OsStr| n.to_str())
                .unwrap_or("my-project");
            sanitize_package_name(dir_name)
        }
    };

    let python_specifier = match &args.python {
        Some(spec) => spec.clone(),
        None => detect_python_specifier(),
    };

    let content = generate_pyproject(&project_name, &python_specifier);

    std::fs::write(&pyproject_path, &content)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to write {}", pyproject_path.display()))?;

    eprintln!(
        "{} Initialized project {} at {}",
        "✓".green().bold(),
        project_name.cyan(),
        pyproject_path.display().to_string().dimmed()
    );

    Ok(())
}

/// Generate a minimal PEP 621 pyproject.toml.
pub fn generate_pyproject(name: &str, python_specifier: &str) -> String {
    format!(
        r#"[project]
name = "{name}"
version = "0.1.0"
description = ""
requires-python = "{python_specifier}"
dependencies = []
"#
    )
}

/// Sanitize a string to a valid PEP 508 / PEP 503 package name.
///
/// - Lowercase
/// - Replace runs of non-alphanumeric characters with a single hyphen
/// - Strip leading/trailing hyphens
fn sanitize_package_name(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    let mut last_was_sep = true; // treat start as separator to strip leading hyphens

    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            result.push(ch.to_ascii_lowercase());
            last_was_sep = false;
        } else if !last_was_sep {
            result.push('-');
            last_was_sep = true;
        }
    }

    // Strip trailing hyphen
    if result.ends_with('-') {
        result.pop();
    }

    if result.is_empty() {
        "my-project".to_string()
    } else {
        result
    }
}

/// Detect the system Python minor version and return a specifier like `>=3.12`.
fn detect_python_specifier() -> String {
    // Try `python3 --version` first, then `python --version`
    for cmd in &["python3", "python"] {
        if let Ok(output) = std::process::Command::new(cmd).arg("--version").output() {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if let Some(version) = parse_python_minor(&stdout) {
                    return format!(">={}", version);
                }
            }
        }
    }

    // Fallback
    ">=3.10".to_string()
}

/// Extract "3.X" from a string like "Python 3.12.1".
fn parse_python_minor(output: &str) -> Option<String> {
    let version_part = output.trim().strip_prefix("Python ")?;
    let parts: Vec<&str> = version_part.split('.').collect();
    if parts.len() >= 2 {
        Some(format!("{}.{}", parts[0], parts[1]))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_pyproject_valid_content() {
        let content = generate_pyproject("my-cool-project", ">=3.12");
        assert!(content.contains("[project]"));
        assert!(content.contains("name = \"my-cool-project\""));
        assert!(content.contains("version = \"0.1.0\""));
        assert!(content.contains("requires-python = \">=3.12\""));
        assert!(content.contains("dependencies = []"));

        // Verify it parses as valid TOML
        let parsed: toml_edit::DocumentMut = content.parse().expect("should be valid TOML");
        let project = parsed["project"].as_table().expect("should have [project]");
        assert_eq!(project["name"].as_str().unwrap(), "my-cool-project");
    }

    #[test]
    fn test_sanitize_package_name() {
        assert_eq!(sanitize_package_name("My_Cool.Package"), "my-cool-package");
        assert_eq!(sanitize_package_name("simple"), "simple");
        assert_eq!(sanitize_package_name("UPPER"), "upper");
        assert_eq!(sanitize_package_name("a--b__c..d"), "a-b-c-d");
        assert_eq!(sanitize_package_name("--leading--"), "leading");
        assert_eq!(sanitize_package_name("!!!"), "my-project");
    }

    #[test]
    fn test_parse_python_minor() {
        assert_eq!(parse_python_minor("Python 3.12.1"), Some("3.12".into()));
        assert_eq!(parse_python_minor("Python 3.10.0"), Some("3.10".into()));
        assert_eq!(parse_python_minor("Python 3.9"), Some("3.9".into()));
        assert_eq!(parse_python_minor("not python"), None);
    }

    #[test]
    fn test_init_errors_if_pyproject_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let pyproject = tmp.path().join("pyproject.toml");
        std::fs::write(&pyproject, "# existing").unwrap();

        let args = InitArgs {
            name: None,
            python: None,
            dir: tmp.path().to_path_buf(),
        };
        let result = cmd_init(args);
        assert!(result.is_err());
        let err = format!("{:?}", result.unwrap_err());
        assert!(err.contains("pyproject.toml already exists"));
    }

    #[test]
    fn test_init_creates_pyproject() {
        let tmp = tempfile::tempdir().unwrap();

        let args = InitArgs {
            name: Some("test-pkg".into()),
            python: Some(">=3.11".into()),
            dir: tmp.path().to_path_buf(),
        };
        cmd_init(args).unwrap();

        let content = std::fs::read_to_string(tmp.path().join("pyproject.toml")).unwrap();
        assert!(content.contains("name = \"test-pkg\""));
        assert!(content.contains("requires-python = \">=3.11\""));
    }
}
