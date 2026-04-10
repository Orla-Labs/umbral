//! PEP 517 sdist building with build isolation.
//!
//! Creates an isolated build environment, installs build dependencies,
//! and invokes the PEP 517 build backend to produce a wheel from source.

use std::path::{Path, PathBuf};
use std::process::Command;

use thiserror::Error;
use tracing::info;

// ── Errors ──────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum BuildError {
    #[error("failed to create build environment: {0}")]
    VenvCreation(String),

    #[error("failed to install build dependencies: {0}")]
    DependencyInstall(String),

    #[error("build backend invocation failed: {0}")]
    BackendFailed(String),

    #[error("no wheel produced by build backend in {0}")]
    NoWheelProduced(PathBuf),

    #[error("no sdist produced by build backend in {0}")]
    NoSdistProduced(PathBuf),

    #[error("build backend not specified in pyproject.toml")]
    NoBuildBackend,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

// ── Configuration ───────────────────────────────────────────────────

/// Configuration for building a wheel from source.
pub struct BuildConfig {
    /// The Python interpreter to use for the build environment.
    pub python: PathBuf,
    /// Build backend (e.g., "setuptools.build_meta").
    pub build_backend: String,
    /// Build system requirements (e.g., ["setuptools>=64", "wheel"]).
    pub requires: Vec<String>,
    /// Optional backend path entries prepended to sys.path.
    pub backend_path: Option<Vec<String>>,
}

// ── Safety helpers ──────────────────────────────────────────────────

/// Escape a string for safe inclusion in a Python single-quoted string literal.
pub fn python_string_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Validate that a build backend name contains only valid Python identifier
/// characters (alphanumeric, underscore, dot for module paths, colon for
/// object access per PEP 517).
pub fn validate_backend_name(name: &str) -> Result<(), BuildError> {
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == ':')
    {
        return Err(BuildError::BackendFailed(format!(
            "invalid build backend name: {}",
            name
        )));
    }
    Ok(())
}

// ── Backend parsing ─────────────────────────────────────────────────

/// Split a PEP 517 build-backend string into (module, optional object).
///
/// Examples:
/// - `"setuptools.build_meta"` -> `("setuptools.build_meta", None)`
/// - `"setuptools.build_meta:__legacy__"` -> `("setuptools.build_meta", Some("__legacy__"))`
pub fn parse_backend(backend: &str) -> (&str, Option<&str>) {
    if let Some(colon_pos) = backend.find(':') {
        (&backend[..colon_pos], Some(&backend[colon_pos + 1..]))
    } else {
        (backend, None)
    }
}

// ── Isolated build environment ──────────────────────────────────────

/// An isolated build environment with a temporary venv and installed
/// build dependencies.
///
/// The venv lives inside `_temp_dir` and is cleaned up on drop.
pub struct IsolatedBuildEnv {
    /// Keep the temp dir alive for the lifetime of this struct.
    _temp_dir: tempfile::TempDir,
    /// Path to the Python interpreter inside the build venv.
    pub python_bin: PathBuf,
}

/// Create a temporary venv, run ensurepip, and install the given build
/// requirements.  Returns a handle whose `python_bin` field points to the
/// interpreter inside the venv.  The temporary directory is cleaned up
/// when the returned struct is dropped.
pub fn create_isolated_build_env(config: &BuildConfig) -> Result<IsolatedBuildEnv, BuildError> {
    let temp_dir = tempfile::tempdir()?;
    let venv_path = temp_dir.path().join("build-env");

    let interpreter = umbral_venv::PythonInterpreter::find(None)
        .map_err(|e| BuildError::VenvCreation(format!("failed to find Python interpreter: {e}")))?;

    umbral_venv::create_venv(&venv_path, &interpreter, Some("umbral-build"))
        .map_err(|e| BuildError::VenvCreation(format!("failed to create build venv: {e}")))?;

    let python_bin = if cfg!(windows) {
        venv_path.join("Scripts").join("python.exe")
    } else {
        venv_path.join("bin").join("python3")
    };

    info!(python = %python_bin.display(), "build environment created");

    // Install build dependencies using pip (bootstrap)
    if !config.requires.is_empty() {
        // First ensure pip is available
        let ensurepip = Command::new(&python_bin)
            .args(["-m", "ensurepip", "--upgrade"])
            .output()?;
        if !ensurepip.status.success() {
            return Err(BuildError::DependencyInstall(format!(
                "ensurepip failed: {}",
                String::from_utf8_lossy(&ensurepip.stderr)
            )));
        }

        // Install build requirements
        let mut pip_args = vec![
            "-m".to_string(),
            "pip".to_string(),
            "install".to_string(),
            "--quiet".to_string(),
        ];
        pip_args.extend(config.requires.iter().cloned());

        let pip_args_refs: Vec<&str> = pip_args.iter().map(|s| s.as_str()).collect();

        info!(requires = ?config.requires, "installing build dependencies");

        let pip = Command::new(&python_bin).args(&pip_args_refs).output()?;
        if !pip.status.success() {
            return Err(BuildError::DependencyInstall(format!(
                "pip install failed: {}",
                String::from_utf8_lossy(&pip.stderr)
            )));
        }
    }

    Ok(IsolatedBuildEnv {
        _temp_dir: temp_dir,
        python_bin,
    })
}

// ── Shared script construction ─────────────────────────────────────

/// Build the common prefix of a PEP 517 build script (sys.path setup +
/// backend import).  Returns `(script_parts, module, caller_prefix)`
/// where `caller_prefix` is e.g. `"setuptools.build_meta"` or
/// `"setuptools.build_meta.__legacy__"`.
fn build_script_prefix(
    config: &BuildConfig,
    source_dir: &Path,
) -> Result<(Vec<String>, String, String), BuildError> {
    let (module, object) = parse_backend(&config.build_backend);
    validate_backend_name(&config.build_backend)?;

    let caller_prefix = if let Some(obj) = object {
        format!("{module}.{obj}")
    } else {
        module.to_string()
    };

    let mut script_parts: Vec<String> = vec!["import sys, os".to_string()];

    // Insert backend_path entries at the front of sys.path
    if let Some(ref paths) = config.backend_path {
        for p in paths.iter().rev() {
            let escaped = python_string_escape(p);
            script_parts.push(format!("sys.path.insert(0, '{escaped}')"));
        }
    }

    // Also insert the source dir so the backend module can find setup.py/setup.cfg
    let source_dir_escaped = python_string_escape(&source_dir.display().to_string());
    script_parts.push(format!("sys.path.insert(0, '{source_dir_escaped}')"));

    script_parts.push(format!("import {module}"));

    Ok((script_parts, module.to_string(), caller_prefix))
}

// ── Build ───────────────────────────────────────────────────────────

/// Build a wheel from a source directory using PEP 517.
///
/// Creates an isolated build environment, installs build deps, and invokes
/// the build backend's `build_wheel()` function.
pub fn build_wheel_from_source(
    source_dir: &Path,
    output_dir: &Path,
    config: &BuildConfig,
) -> Result<PathBuf, BuildError> {
    info!(
        source = %source_dir.display(),
        backend = %config.build_backend,
        "building wheel from source (PEP 517)"
    );

    // 1. Create isolated build environment
    let env = create_isolated_build_env(config)?;

    // 2. Create output directory
    std::fs::create_dir_all(output_dir)?;

    // 3. Invoke the build backend's build_wheel()
    let (mut script_parts, _module, caller_prefix) = build_script_prefix(config, source_dir)?;

    let output_dir_escaped = python_string_escape(&output_dir.display().to_string());
    script_parts.push(format!(
        "wheel_name = {caller_prefix}.build_wheel('{output_dir_escaped}')"
    ));
    script_parts.push("print(wheel_name)".to_string());

    let full_script = script_parts.join("; ");

    info!(backend = %config.build_backend, "invoking build backend");

    let build = Command::new(&env.python_bin)
        .args(["-c", &full_script])
        .current_dir(source_dir)
        .output()?;

    if !build.status.success() {
        return Err(BuildError::BackendFailed(
            String::from_utf8_lossy(&build.stderr).to_string(),
        ));
    }

    // 4. Find the wheel in output_dir
    let wheel_name = String::from_utf8_lossy(&build.stdout).trim().to_string();
    let wheel_path = output_dir.join(&wheel_name);

    if wheel_path.exists() {
        info!(wheel = %wheel_path.display(), "wheel built successfully");
        Ok(wheel_path)
    } else {
        // Fall back to scanning directory for .whl files
        for entry in std::fs::read_dir(output_dir)? {
            let entry = entry?;
            if entry.path().extension().is_some_and(|ext| ext == "whl") {
                let found = entry.path();
                info!(wheel = %found.display(), "wheel built successfully (found by scan)");
                return Ok(found);
            }
        }
        Err(BuildError::NoWheelProduced(output_dir.to_path_buf()))
    }
}

/// Build an sdist from a source directory using PEP 517.
///
/// Creates an isolated build environment, installs build deps, and invokes
/// the build backend's `build_sdist()` function.
/// Returns the path to the built sdist (`.tar.gz`).
pub fn build_sdist_from_source(
    source_dir: &Path,
    output_dir: &Path,
    config: &BuildConfig,
) -> Result<PathBuf, BuildError> {
    info!(
        source = %source_dir.display(),
        backend = %config.build_backend,
        "building sdist from source (PEP 517)"
    );

    // 1. Create isolated build environment
    let env = create_isolated_build_env(config)?;

    // 2. Create output directory
    std::fs::create_dir_all(output_dir)?;

    // 3. Invoke the build backend's build_sdist()
    let (mut script_parts, _module, caller_prefix) = build_script_prefix(config, source_dir)?;

    let output_dir_escaped = python_string_escape(&output_dir.display().to_string());
    script_parts.push(format!(
        "name = {caller_prefix}.build_sdist('{output_dir_escaped}')"
    ));
    script_parts.push("print(name)".to_string());

    let full_script = script_parts.join("; ");

    info!(backend = %config.build_backend, "invoking build backend");

    let build = Command::new(&env.python_bin)
        .args(["-c", &full_script])
        .current_dir(source_dir)
        .output()?;

    if !build.status.success() {
        return Err(BuildError::BackendFailed(
            String::from_utf8_lossy(&build.stderr).to_string(),
        ));
    }

    // 4. Find the sdist in output_dir
    let sdist_name = String::from_utf8_lossy(&build.stdout).trim().to_string();
    let sdist_path = output_dir.join(&sdist_name);

    if sdist_path.exists() {
        info!(sdist = %sdist_path.display(), "sdist built successfully");
        Ok(sdist_path)
    } else {
        // Fall back to scanning directory for .tar.gz files
        for entry in std::fs::read_dir(output_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "gz")
                && path.to_string_lossy().contains(".tar.")
            {
                info!(sdist = %path.display(), "sdist built successfully (found by scan)");
                return Ok(path);
            }
        }
        Err(BuildError::NoSdistProduced(output_dir.to_path_buf()))
    }
}

/// Extract a `.tar.gz` sdist archive to a destination directory.
///
/// Returns the path to the extracted source directory (the single top-level
/// directory inside the tarball, per sdist convention).
pub fn extract_sdist(sdist_path: &Path, dest_dir: &Path) -> Result<PathBuf, BuildError> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let file = std::fs::File::open(sdist_path)?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);
    archive.unpack(dest_dir)?;

    // The sdist convention is a single top-level directory named
    // `<name>-<version>/`. Find it.
    for entry in std::fs::read_dir(dest_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            return Ok(entry.path());
        }
    }

    // If no directory found, the source is directly in dest_dir
    Ok(dest_dir.to_path_buf())
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_backend_simple() {
        let (module, object) = parse_backend("setuptools.build_meta");
        assert_eq!(module, "setuptools.build_meta");
        assert_eq!(object, None);
    }

    #[test]
    fn test_parse_backend_with_object() {
        let (module, object) = parse_backend("setuptools.build_meta:__legacy__");
        assert_eq!(module, "setuptools.build_meta");
        assert_eq!(object, Some("__legacy__"));
    }

    #[test]
    fn test_parse_backend_flit() {
        let (module, object) = parse_backend("flit_core.buildapi");
        assert_eq!(module, "flit_core.buildapi");
        assert_eq!(object, None);
    }

    #[test]
    fn test_parse_backend_hatchling() {
        let (module, object) = parse_backend("hatchling.build");
        assert_eq!(module, "hatchling.build");
        assert_eq!(object, None);
    }

    #[test]
    fn test_parse_backend_maturin() {
        let (module, object) = parse_backend("maturin:import_hook");
        assert_eq!(module, "maturin");
        assert_eq!(object, Some("import_hook"));
    }

    #[test]
    fn test_build_config_parses_backend() {
        // Test that BuildConfig with "setuptools.build_meta:__legacy__" works
        let config = BuildConfig {
            python: PathBuf::from("/usr/bin/python3"),
            build_backend: "setuptools.build_meta:__legacy__".to_string(),
            requires: vec!["setuptools".to_string()],
            backend_path: None,
        };
        let (module, object) = parse_backend(&config.build_backend);
        assert_eq!(module, "setuptools.build_meta");
        assert_eq!(object, Some("__legacy__"));
    }

    #[test]
    #[ignore] // Requires a system Python with setuptools
    fn test_build_wheel_from_source() {
        let tmp = tempfile::tempdir().unwrap();
        let source_dir = tmp.path().join("pkg");
        let pkg_dir = source_dir.join("test_pkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();

        // Create a minimal Python package
        std::fs::write(
            source_dir.join("pyproject.toml"),
            r#"[build-system]
requires = ["setuptools"]
build-backend = "setuptools.build_meta"

[project]
name = "test-pkg"
version = "0.1.0"
"#,
        )
        .unwrap();

        std::fs::write(pkg_dir.join("__init__.py"), "").unwrap();

        let output_dir = tmp.path().join("output");

        let config = BuildConfig {
            python: PathBuf::from("python3"),
            build_backend: "setuptools.build_meta".to_string(),
            requires: vec!["setuptools".to_string()],
            backend_path: None,
        };

        let wheel_path = build_wheel_from_source(&source_dir, &output_dir, &config).unwrap();
        assert!(wheel_path.exists());
        assert!(
            wheel_path.extension().is_some_and(|ext| ext == "whl"),
            "expected .whl file, got: {}",
            wheel_path.display()
        );
    }

    #[test]
    #[ignore] // Requires a Python interpreter
    fn test_build_error_bad_backend() {
        let tmp = tempfile::tempdir().unwrap();
        let source_dir = tmp.path().join("pkg");
        std::fs::create_dir_all(&source_dir).unwrap();

        std::fs::write(
            source_dir.join("pyproject.toml"),
            r#"[build-system]
requires = ["setuptools"]
build-backend = "nonexistent_backend_module_xyz"

[project]
name = "test-pkg"
version = "0.1.0"
"#,
        )
        .unwrap();

        let output_dir = tmp.path().join("output");

        let config = BuildConfig {
            python: PathBuf::from("python3"),
            build_backend: "nonexistent_backend_module_xyz".to_string(),
            requires: vec!["setuptools".to_string()],
            backend_path: None,
        };

        let result = build_wheel_from_source(&source_dir, &output_dir, &config);
        assert!(result.is_err());
        match result.unwrap_err() {
            BuildError::BackendFailed(msg) => {
                assert!(
                    msg.contains("nonexistent_backend_module_xyz")
                        || msg.contains("ModuleNotFoundError")
                        || msg.contains("No module named"),
                    "unexpected error message: {msg}"
                );
            }
            other => panic!("expected BackendFailed, got: {other:?}"),
        }
    }

    #[test]
    #[ignore] // Requires a system Python with setuptools
    fn test_build_sdist_from_source() {
        let tmp = tempfile::tempdir().unwrap();
        let source_dir = tmp.path().join("pkg");
        let pkg_dir = source_dir.join("test_pkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();

        // Create a minimal Python package
        std::fs::write(
            source_dir.join("pyproject.toml"),
            r#"[build-system]
requires = ["setuptools"]
build-backend = "setuptools.build_meta"

[project]
name = "test-pkg"
version = "0.1.0"
"#,
        )
        .unwrap();

        std::fs::write(pkg_dir.join("__init__.py"), "").unwrap();

        let output_dir = tmp.path().join("output");

        let config = BuildConfig {
            python: PathBuf::from("python3"),
            build_backend: "setuptools.build_meta".to_string(),
            requires: vec!["setuptools".to_string()],
            backend_path: None,
        };

        let sdist_path = build_sdist_from_source(&source_dir, &output_dir, &config).unwrap();
        assert!(sdist_path.exists());
        assert!(
            sdist_path.to_string_lossy().ends_with(".tar.gz"),
            "expected .tar.gz file, got: {}",
            sdist_path.display()
        );
    }

    #[test]
    fn test_build_error_no_backend() {
        // The NoBuildBackend error is for use by callers that check for
        // a missing build_backend before calling build_wheel_from_source.
        let err = BuildError::NoBuildBackend;
        assert_eq!(
            err.to_string(),
            "build backend not specified in pyproject.toml"
        );
    }

    #[test]
    fn test_build_error_no_sdist_produced() {
        let err = BuildError::NoSdistProduced(PathBuf::from("/tmp/output"));
        assert_eq!(
            err.to_string(),
            "no sdist produced by build backend in /tmp/output"
        );
    }

    #[test]
    fn test_build_script_prefix() {
        let config = BuildConfig {
            python: PathBuf::from("python3"),
            build_backend: "setuptools.build_meta".to_string(),
            requires: vec![],
            backend_path: None,
        };
        let source = PathBuf::from("/tmp/src");
        let (parts, module, caller_prefix) = build_script_prefix(&config, &source).unwrap();

        assert_eq!(module, "setuptools.build_meta");
        assert_eq!(caller_prefix, "setuptools.build_meta");
        assert!(parts.contains(&"import sys, os".to_string()));
        assert!(parts.contains(&"import setuptools.build_meta".to_string()));
    }

    #[test]
    fn test_build_script_prefix_with_object() {
        let config = BuildConfig {
            python: PathBuf::from("python3"),
            build_backend: "setuptools.build_meta:__legacy__".to_string(),
            requires: vec![],
            backend_path: None,
        };
        let source = PathBuf::from("/tmp/src");
        let (_parts, module, caller_prefix) = build_script_prefix(&config, &source).unwrap();

        assert_eq!(module, "setuptools.build_meta");
        assert_eq!(caller_prefix, "setuptools.build_meta.__legacy__");
    }

    #[test]
    fn test_build_script_prefix_with_backend_path() {
        let config = BuildConfig {
            python: PathBuf::from("python3"),
            build_backend: "my_backend".to_string(),
            requires: vec![],
            backend_path: Some(vec!["/custom/path".to_string()]),
        };
        let source = PathBuf::from("/tmp/src");
        let (parts, _module, _caller_prefix) = build_script_prefix(&config, &source).unwrap();

        // Should have the custom path inserted
        let has_custom_path = parts.iter().any(|p| p.contains("/custom/path"));
        assert!(has_custom_path, "expected backend_path in script parts");
    }

    #[test]
    fn test_extract_sdist() {
        // Create a fake .tar.gz with the expected structure
        let tmp = tempfile::tempdir().unwrap();
        let sdist_path = tmp.path().join("test-pkg-0.1.0.tar.gz");
        let dest_dir = tmp.path().join("extracted");
        std::fs::create_dir_all(&dest_dir).unwrap();

        // Build a tar.gz in memory
        {
            use flate2::write::GzEncoder;
            use flate2::Compression;
            let file = std::fs::File::create(&sdist_path).unwrap();
            let enc = GzEncoder::new(file, Compression::default());
            let mut tar_builder = tar::Builder::new(enc);

            // Add a directory entry
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_mode(0o755);
            header.set_cksum();
            tar_builder
                .append_data(&mut header, "test-pkg-0.1.0/", &[] as &[u8])
                .unwrap();

            // Add a file
            let content = b"[build-system]\nrequires = [\"setuptools\"]\n";
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar_builder
                .append_data(&mut header, "test-pkg-0.1.0/pyproject.toml", &content[..])
                .unwrap();

            tar_builder.finish().unwrap();
        }

        let source_dir = extract_sdist(&sdist_path, &dest_dir).unwrap();
        assert!(source_dir.exists());
        assert!(source_dir.join("pyproject.toml").exists());
    }

    #[test]
    fn test_python_string_escape() {
        assert_eq!(
            python_string_escape("path with 'quotes'"),
            "path with \\'quotes\\'"
        );
    }

    #[test]
    fn test_python_string_escape_backslashes() {
        assert_eq!(
            python_string_escape("C:\\Users\\test"),
            "C:\\\\Users\\\\test"
        );
    }

    #[test]
    fn test_validate_backend_name_valid() {
        assert!(validate_backend_name("setuptools.build_meta").is_ok());
    }

    #[test]
    fn test_validate_backend_name_with_colon() {
        assert!(validate_backend_name("setuptools.build_meta:__legacy__").is_ok());
    }

    #[test]
    fn test_validate_backend_name_invalid() {
        let result = validate_backend_name("\"; import os");
        assert!(result.is_err());
        match result.unwrap_err() {
            BuildError::BackendFailed(msg) => {
                assert!(msg.contains("invalid build backend name"));
            }
            other => panic!("expected BackendFailed, got: {other:?}"),
        }
    }
}
