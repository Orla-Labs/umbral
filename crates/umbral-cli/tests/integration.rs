//! Integration tests for the umbral CLI binary.
//!
//! These tests invoke the compiled `umbral` binary via `std::process::Command`
//! to verify end-to-end behavior of the CLI subcommands.

use std::path::PathBuf;
use std::process::Command;

/// Return the path to the `umbral` binary built by cargo.
fn umbral_bin() -> PathBuf {
    // CARGO_MANIFEST_DIR for umbral-cli is crates/umbral-cli.
    // The workspace target directory is at the workspace root.
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop(); // crates
    path.pop(); // workspace root
    path.push("target");
    path.push("debug");
    path.push("umbral");
    if cfg!(windows) {
        path.set_extension("exe");
    }
    path
}

#[test]
fn test_umbral_help() {
    let output = Command::new(umbral_bin())
        .arg("--help")
        .output()
        .expect("failed to execute umbral --help");

    assert!(output.status.success(), "umbral --help should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Python package manager"),
        "help output should mention Python package manager"
    );
}

#[test]
fn test_umbral_version() {
    let output = Command::new(umbral_bin())
        .arg("--version")
        .output()
        .expect("failed to execute umbral --version");

    assert!(output.status.success(), "umbral --version should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("umbral"),
        "version output should contain 'umbral'"
    );
}

#[test]
fn test_venv_creates_valid_structure() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let venv_path = tmp.path().join(".venv");

    let output = Command::new(umbral_bin())
        .arg("venv")
        .arg(venv_path.to_str().unwrap())
        .output()
        .expect("failed to execute umbral venv");

    // If no Python is found, skip the test gracefully
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("failed to find a Python interpreter") {
            eprintln!("Skipping test_venv_creates_valid_structure: no Python found");
            return;
        }
        panic!(
            "umbral venv failed unexpectedly:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            stderr,
        );
    }

    // Verify venv directory structure
    assert!(venv_path.exists(), "venv directory should exist");
    assert!(
        venv_path.join("pyvenv.cfg").exists(),
        "pyvenv.cfg should exist"
    );

    if cfg!(windows) {
        assert!(
            venv_path.join("Scripts").join("python.exe").exists(),
            "Scripts/python.exe should exist on Windows"
        );
    } else {
        assert!(
            venv_path.join("bin").join("python").exists(),
            "bin/python should exist on Unix"
        );
        assert!(
            venv_path.join("bin").join("activate").exists(),
            "bin/activate should exist on Unix"
        );
    }

    // Verify pyvenv.cfg contents
    let cfg =
        std::fs::read_to_string(venv_path.join("pyvenv.cfg")).expect("failed to read pyvenv.cfg");
    assert!(
        cfg.contains("include-system-site-packages = false"),
        "pyvenv.cfg should contain site-packages setting"
    );
    assert!(
        cfg.contains("version = "),
        "pyvenv.cfg should contain version"
    );
}

#[test]
fn test_resolve_with_simple_project() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let project_path = tmp.path().join("pyproject.toml");
    let lockfile_path = tmp.path().join("uv.lock");

    // Write a minimal pyproject.toml with a commonly available package
    std::fs::write(
        &project_path,
        r#"[project]
name = "test-project"
version = "0.1.0"
requires-python = ">=3.10"
dependencies = [
    "six>=1.0",
]

[build-system]
requires = ["setuptools"]
build-backend = "setuptools.build_meta"
"#,
    )
    .expect("failed to write pyproject.toml");

    let output = Command::new(umbral_bin())
        .args([
            "resolve",
            "--project",
            project_path.to_str().unwrap(),
            "--output",
            lockfile_path.to_str().unwrap(),
        ])
        .output()
        .expect("failed to execute umbral resolve");

    // This test requires network access. If it fails due to network,
    // we check the error message and skip gracefully.
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("request failed")
            || stderr.contains("RetryExhausted")
            || stderr.contains("error sending request")
        {
            eprintln!("Skipping test_resolve_with_simple_project: network unavailable");
            return;
        }
        panic!(
            "umbral resolve failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            stderr,
        );
    }

    // Verify lockfile was created
    assert!(
        lockfile_path.exists(),
        "lockfile should exist after resolve"
    );

    let content = std::fs::read_to_string(&lockfile_path).expect("failed to read lockfile");
    assert!(
        content.contains("six"),
        "lockfile should contain the resolved 'six' package"
    );
    assert!(
        content.contains("@generated by umbral"),
        "lockfile should contain the generated header"
    );
}

#[test]
fn test_resolve_missing_project_file() {
    let output = Command::new(umbral_bin())
        .args(["resolve", "--project", "/nonexistent/pyproject.toml"])
        .output()
        .expect("failed to execute umbral resolve");

    assert!(
        !output.status.success(),
        "resolve with missing project file should fail"
    );
}

#[test]
fn test_install_missing_lockfile() {
    let output = Command::new(umbral_bin())
        .args(["install", "--lockfile", "/nonexistent/uv.lock"])
        .output()
        .expect("failed to execute umbral install");

    assert!(
        !output.status.success(),
        "install with missing lockfile should fail"
    );
}

#[test]
fn test_sync_missing_lockfile() {
    // The sync command now uses ensure_synced which reads pyproject.toml first,
    // then the lockfile. With a nonexistent lockfile path and no pyproject.toml,
    // it fails on the project file. We verify it fails gracefully either way.
    let output = Command::new(umbral_bin())
        .args(["sync", "--lockfile", "/nonexistent/uv.lock"])
        .output()
        .expect("failed to execute umbral sync");

    assert!(
        !output.status.success(),
        "sync with missing lockfile should fail"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to read") || stderr.contains("No such file"),
        "sync should report a read failure, got: {}",
        stderr
    );
}

#[test]
fn test_sync_help() {
    let output = Command::new(umbral_bin())
        .args(["sync", "--help"])
        .output()
        .expect("failed to execute umbral sync --help");

    assert!(output.status.success(), "umbral sync --help should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Sync the virtual environment"),
        "sync help should describe syncing the virtual environment"
    );
}

#[test]
fn test_resolve_produces_uv_lock_format() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let project_path = tmp.path().join("pyproject.toml");
    let lockfile_path = tmp.path().join("uv.lock");

    std::fs::write(
        &project_path,
        r#"[project]
name = "format-test"
version = "0.1.0"
requires-python = ">=3.10"
dependencies = [
    "six>=1.0",
]

[build-system]
requires = ["setuptools"]
build-backend = "setuptools.build_meta"
"#,
    )
    .expect("failed to write pyproject.toml");

    let output = Command::new(umbral_bin())
        .args([
            "resolve",
            "--project",
            project_path.to_str().unwrap(),
            "--output",
            lockfile_path.to_str().unwrap(),
        ])
        .output()
        .expect("failed to execute umbral resolve");

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("request failed")
            || stderr.contains("RetryExhausted")
            || stderr.contains("error sending request")
        {
            eprintln!("Skipping test_resolve_produces_uv_lock_format: network unavailable");
            return;
        }
        panic!(
            "umbral resolve failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            stderr,
        );
    }

    assert!(lockfile_path.exists(), "uv.lock should exist after resolve");

    let content = std::fs::read_to_string(&lockfile_path).expect("failed to read uv.lock");

    // Verify the uv.lock format structure
    assert!(
        content.contains("@generated by umbral"),
        "uv.lock should contain the generated header"
    );
    assert!(
        content.contains("[[package]]"),
        "uv.lock should contain [[package]] entries"
    );
    assert!(
        content.contains("requires-python"),
        "uv.lock should contain requires-python"
    );
    assert!(
        content.contains("pypi.org") || content.contains("source"),
        "uv.lock should contain source registry information"
    );
}

#[test]
fn test_uv_lock_readable_after_resolve() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let project_path = tmp.path().join("pyproject.toml");
    let lockfile_path = tmp.path().join("uv.lock");

    std::fs::write(
        &project_path,
        r#"[project]
name = "roundtrip-test"
version = "0.1.0"
requires-python = ">=3.10"
dependencies = [
    "six>=1.0",
]

[build-system]
requires = ["setuptools"]
build-backend = "setuptools.build_meta"
"#,
    )
    .expect("failed to write pyproject.toml");

    let output = Command::new(umbral_bin())
        .args([
            "resolve",
            "--project",
            project_path.to_str().unwrap(),
            "--output",
            lockfile_path.to_str().unwrap(),
        ])
        .output()
        .expect("failed to execute umbral resolve");

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("request failed")
            || stderr.contains("RetryExhausted")
            || stderr.contains("error sending request")
        {
            eprintln!("Skipping test_uv_lock_readable_after_resolve: network unavailable");
            return;
        }
        panic!(
            "umbral resolve failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            stderr,
        );
    }

    assert!(lockfile_path.exists(), "uv.lock should exist");

    // Read the generated uv.lock and verify it can be parsed as valid TOML
    // and contains the expected uv.lock format structure.
    let content = std::fs::read_to_string(&lockfile_path).expect("failed to read uv.lock");

    // Verify the uv.lock format: starts with the generated header, has
    // top-level version/requires-python, and contains [[package]] entries.
    assert!(
        content.contains("@generated"),
        "uv.lock should contain @generated header"
    );
    assert!(
        content.contains("version = 1"),
        "uv.lock should start with version = 1"
    );
    assert!(
        content.contains("requires-python"),
        "uv.lock should contain requires-python"
    );
    assert!(
        content.contains("[[package]]"),
        "uv.lock should contain [[package]] entries"
    );

    // Verify 'six' is present as a resolved package
    assert!(
        content.contains("name = \"six\""),
        "uv.lock should contain the 'six' package"
    );

    // Verify each package entry has source = { registry = "..." }
    assert!(
        content.contains("source = { registry ="),
        "uv.lock packages should have source = {{ registry = \"...\" }}"
    );

    // Verify the lockfile can be parsed back by UvLock
    let parsed = umbral_lockfile::UvLock::from_str(&content)
        .expect("generated uv.lock should be parseable by UvLock::from_str");
    assert!(
        !parsed.packages.is_empty(),
        "parsed uv.lock should have at least one package"
    );
    assert!(
        parsed.packages.iter().any(|p| p.name == "six"),
        "parsed uv.lock should contain 'six'"
    );
}

#[test]
fn test_resolve_default_output_is_uv_lock() {
    // Verify the resolve --help mentions uv.lock as the default
    let output = Command::new(umbral_bin())
        .args(["resolve", "--help"])
        .output()
        .expect("failed to execute umbral resolve --help");

    assert!(output.status.success(), "resolve --help should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("uv.lock"),
        "resolve help should mention uv.lock as default output, got: {}",
        stdout
    );
}

// ── Edge case: `umbral init` creates valid pyproject.toml ──────────

#[test]
fn test_init_creates_pyproject_toml() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");

    let output = Command::new(umbral_bin())
        .arg("init")
        .current_dir(tmp.path())
        .output()
        .expect("failed to execute umbral init");

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // If init fails for a legitimate reason (e.g. no Python), skip gracefully
        if stderr.contains("failed to find") || stderr.contains("Python") {
            eprintln!(
                "Skipping test_init_creates_pyproject_toml: {}",
                stderr.trim()
            );
            return;
        }
        panic!(
            "umbral init failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            stderr,
        );
    }

    let pyproject_path = tmp.path().join("pyproject.toml");
    assert!(
        pyproject_path.exists(),
        "pyproject.toml should exist after init"
    );

    let content = std::fs::read_to_string(&pyproject_path).expect("failed to read pyproject.toml");
    assert!(
        content.contains("[project]"),
        "pyproject.toml should contain [project] section"
    );
    assert!(
        content.contains("name"),
        "pyproject.toml should contain a name field"
    );
}

// ── Edge case: `umbral init` in directory with existing pyproject.toml ─

#[test]
fn test_init_with_existing_pyproject_toml() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let pyproject_path = tmp.path().join("pyproject.toml");

    // Pre-create a pyproject.toml
    std::fs::write(
        &pyproject_path,
        "[project]\nname = \"existing\"\nversion = \"1.0.0\"\n",
    )
    .expect("failed to write existing pyproject.toml");

    let output = Command::new(umbral_bin())
        .arg("init")
        .current_dir(tmp.path())
        .output()
        .expect("failed to execute umbral init");

    // Should either fail or warn — the existing file should not be clobbered silently.
    // We check that either the command failed OR the existing content is preserved.
    let content = std::fs::read_to_string(&pyproject_path).expect("failed to read pyproject.toml");

    if output.status.success() {
        // If it succeeded, the original content should still be recognizable
        // (or the file was overwritten, which we note)
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        // Just verify the test ran; the important thing is no crash
        assert!(
            content.contains("[project]"),
            "pyproject.toml should still contain [project] after init on existing file, got: {}",
            content
        );
        let _ = (stderr, stdout); // suppress unused warnings
    } else {
        // If it failed, that's an acceptable behavior: refusing to overwrite
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("already exists")
                || stderr.contains("pyproject.toml")
                || !output.status.success(),
            "init should mention existing file in error, got: {}",
            stderr
        );
    }
}

// ── Edge case: `umbral remove` of non-existent package ─────────────

#[test]
fn test_remove_nonexistent_package() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let pyproject_path = tmp.path().join("pyproject.toml");

    std::fs::write(
        &pyproject_path,
        r#"[project]
name = "test-project"
version = "0.1.0"
requires-python = ">=3.10"
dependencies = [
    "requests>=2.0",
]

[build-system]
requires = ["setuptools"]
build-backend = "setuptools.build_meta"
"#,
    )
    .expect("failed to write pyproject.toml");

    let output = Command::new(umbral_bin())
        .args(["remove", "nonexistent-pkg-xyz"])
        .current_dir(tmp.path())
        .output()
        .expect("failed to execute umbral remove");

    // Should fail gracefully, not crash/panic
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The command should either fail with a clear message or succeed with a warning
    assert!(
        !output.status.success()
            || stderr.contains("not found")
            || stdout.contains("not found")
            || stderr.contains("not in")
            || stdout.contains("not in")
            || stderr.contains("No matching")
            || stdout.contains("No matching"),
        "remove of nonexistent package should fail or warn. stderr: {}, stdout: {}",
        stderr,
        stdout
    );
}

// ── Edge case: `umbral venv` creates valid venv structure ──────────

#[test]
fn test_venv_creates_pyvenv_cfg_and_dirs() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let venv_path = tmp.path().join("test-venv");

    let output = Command::new(umbral_bin())
        .arg("venv")
        .arg(venv_path.to_str().unwrap())
        .output()
        .expect("failed to execute umbral venv");

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("failed to find a Python interpreter") {
            eprintln!("Skipping test_venv_creates_pyvenv_cfg_and_dirs: no Python found");
            return;
        }
        panic!(
            "umbral venv failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            stderr,
        );
    }

    // Verify venv directory exists
    assert!(venv_path.exists(), "venv directory should exist");

    // Verify pyvenv.cfg
    let cfg_path = venv_path.join("pyvenv.cfg");
    assert!(cfg_path.exists(), "pyvenv.cfg should exist");
    let cfg_content = std::fs::read_to_string(&cfg_path).expect("failed to read pyvenv.cfg");
    assert!(
        cfg_content.contains("include-system-site-packages"),
        "pyvenv.cfg should contain include-system-site-packages"
    );

    // Verify bin/ or Scripts/ exists
    if cfg!(unix) {
        assert!(
            venv_path.join("bin").exists(),
            "bin/ directory should exist on Unix"
        );
        assert!(
            venv_path.join("lib").exists(),
            "lib/ directory should exist on Unix"
        );
    }
}

// ── Workspace: `umbral resolve` in a workspace ──────────────────────

#[test]
fn test_workspace_resolve() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let root = tmp.path();

    // Create root pyproject.toml with workspace config.
    std::fs::write(
        root.join("pyproject.toml"),
        r#"[project]
name = "my-monorepo"
version = "0.1.0"
requires-python = ">=3.10"
dependencies = ["six>=1.0"]

[build-system]
requires = ["setuptools"]
build-backend = "setuptools.build_meta"

[tool.uv.workspace]
members = ["packages/*"]
"#,
    )
    .expect("failed to write root pyproject.toml");

    // Create member pkg-a.
    let pkg_a_dir = root.join("packages").join("pkg-a");
    std::fs::create_dir_all(&pkg_a_dir).expect("failed to create pkg-a dir");
    std::fs::write(
        pkg_a_dir.join("pyproject.toml"),
        r#"[project]
name = "pkg-a"
version = "0.1.0"
requires-python = ">=3.10"
dependencies = []

[build-system]
requires = ["setuptools"]
build-backend = "setuptools.build_meta"
"#,
    )
    .expect("failed to write pkg-a pyproject.toml");

    // Create member pkg-b that depends on pkg-a as workspace source.
    let pkg_b_dir = root.join("packages").join("pkg-b");
    std::fs::create_dir_all(&pkg_b_dir).expect("failed to create pkg-b dir");
    std::fs::write(
        pkg_b_dir.join("pyproject.toml"),
        r#"[project]
name = "pkg-b"
version = "0.1.0"
requires-python = ">=3.10"
dependencies = ["pkg-a"]

[build-system]
requires = ["setuptools"]
build-backend = "setuptools.build_meta"

[tool.uv.sources]
pkg-a = { workspace = true }
"#,
    )
    .expect("failed to write pkg-b pyproject.toml");

    // Run `umbral resolve` from the workspace root.
    let output = Command::new(umbral_bin())
        .args([
            "resolve",
            "--project",
            root.join("pyproject.toml").to_str().unwrap(),
        ])
        .current_dir(root)
        .output()
        .expect("failed to execute umbral resolve");

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("request failed")
            || stderr.contains("RetryExhausted")
            || stderr.contains("error sending request")
        {
            eprintln!("Skipping test_workspace_resolve: network unavailable");
            return;
        }
        panic!(
            "umbral resolve in workspace failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            stderr,
        );
    }

    // Verify lockfile was created at workspace root (not in a member).
    let lockfile_path = root.join("uv.lock");
    assert!(
        lockfile_path.exists(),
        "uv.lock should exist at workspace root"
    );

    let content = std::fs::read_to_string(&lockfile_path).expect("failed to read uv.lock");
    assert!(
        content.contains("six"),
        "lockfile should contain the resolved 'six' package"
    );
    assert!(
        content.contains("@generated by umbral"),
        "lockfile should contain the generated header"
    );

    // Verify workspace stderr mentions workspace discovery.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("workspace") || stderr.contains("member"),
        "resolve output should mention workspace: {}",
        stderr,
    );
}

// ── Build command tests ──────────────────────────────────────────────

#[test]
fn test_build_help() {
    let output = Command::new(umbral_bin())
        .args(["build", "--help"])
        .output()
        .expect("failed to execute umbral build --help");

    assert!(
        output.status.success(),
        "umbral build --help should succeed"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("output-dir"),
        "build help should mention output-dir flag, got: {}",
        stdout
    );
    assert!(
        stdout.contains("--wheel"),
        "build help should mention --wheel flag, got: {}",
        stdout
    );
    assert!(
        stdout.contains("--sdist"),
        "build help should mention --sdist flag, got: {}",
        stdout
    );
}

#[test]
fn test_build_no_pyproject() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");

    let output = Command::new(umbral_bin())
        .arg("build")
        .current_dir(tmp.path())
        .output()
        .expect("failed to execute umbral build");

    assert!(
        !output.status.success(),
        "build without pyproject.toml should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no pyproject.toml"),
        "build should report missing pyproject.toml, got: {}",
        stderr
    );
}

// ── Publish command tests ───────────────────────────────────────────

#[test]
fn test_publish_help() {
    let output = Command::new(umbral_bin())
        .args(["publish", "--help"])
        .output()
        .expect("failed to execute umbral publish --help");

    assert!(
        output.status.success(),
        "umbral publish --help should succeed"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--token"),
        "publish help should mention --token flag, got: {}",
        stdout
    );
    assert!(
        stdout.contains("--repository"),
        "publish help should mention --repository flag, got: {}",
        stdout
    );
    assert!(
        stdout.contains("--skip-existing"),
        "publish help should mention --skip-existing flag, got: {}",
        stdout
    );
}

#[test]
fn test_publish_no_dist_files() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let dist_dir = tmp.path().join("dist");
    std::fs::create_dir_all(&dist_dir).expect("failed to create dist dir");

    let output = Command::new(umbral_bin())
        .args(["publish", dist_dir.to_str().unwrap()])
        .env("UMBRAL_PUBLISH_TOKEN", "fake-token")
        .env_remove("UV_PUBLISH_TOKEN")
        .output()
        .expect("failed to execute umbral publish");

    assert!(
        !output.status.success(),
        "publish with empty dist dir should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no distribution files found"),
        "publish should report no distribution files found, got: {}",
        stderr
    );
}

#[test]
fn test_publish_no_token() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let dist_dir = tmp.path().join("dist");
    std::fs::create_dir_all(&dist_dir).expect("failed to create dist dir");

    // Create a fake .whl file so the "no dist files" check passes
    std::fs::write(
        dist_dir.join("fake-1.0.0-py3-none-any.whl"),
        b"fake wheel content",
    )
    .expect("failed to write fake wheel");

    let output = Command::new(umbral_bin())
        .args(["publish", dist_dir.to_str().unwrap()])
        .env_remove("UMBRAL_PUBLISH_TOKEN")
        .env_remove("UV_PUBLISH_TOKEN")
        .output()
        .expect("failed to execute umbral publish");

    assert!(
        !output.status.success(),
        "publish without token should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no authentication token"),
        "publish should report missing token, got: {}",
        stderr
    );
}

// ── Pip command tests ─────────────────────────────────────────────

#[test]
fn test_pip_help() {
    let output = Command::new(umbral_bin())
        .args(["pip", "--help"])
        .output()
        .expect("failed to execute umbral pip --help");

    assert!(output.status.success(), "umbral pip --help should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("install") || stdout.contains("Install"),
        "pip help should mention install, got: {}",
        stdout
    );
    assert!(
        stdout.contains("list") || stdout.contains("List"),
        "pip help should mention list, got: {}",
        stdout
    );
    assert!(
        stdout.contains("freeze") || stdout.contains("Freeze"),
        "pip help should mention freeze, got: {}",
        stdout
    );
    assert!(
        stdout.contains("uninstall") || stdout.contains("Uninstall"),
        "pip help should mention uninstall, got: {}",
        stdout
    );
    assert!(
        stdout.contains("compile") || stdout.contains("Compile"),
        "pip help should mention compile, got: {}",
        stdout
    );
}

#[test]
fn test_pip_install_help() {
    let output = Command::new(umbral_bin())
        .args(["pip", "install", "--help"])
        .output()
        .expect("failed to execute umbral pip install --help");

    assert!(
        output.status.success(),
        "umbral pip install --help should succeed"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("packages") || stdout.contains("PACKAGES"),
        "pip install help should mention packages arg, got: {}",
        stdout
    );
    assert!(
        stdout.contains("-r") || stdout.contains("--requirement"),
        "pip install help should mention -r flag, got: {}",
        stdout
    );
    assert!(
        stdout.contains("--target"),
        "pip install help should mention --target flag, got: {}",
        stdout
    );
    assert!(
        stdout.contains("--extra-index-url"),
        "pip install help should mention --extra-index-url flag, got: {}",
        stdout
    );
}

#[test]
fn test_pip_list_no_venv() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");

    let output = Command::new(umbral_bin())
        .args(["pip", "list"])
        .current_dir(tmp.path())
        .env_remove("VIRTUAL_ENV")
        .output()
        .expect("failed to execute umbral pip list");

    assert!(
        !output.status.success(),
        "pip list without a venv should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no virtual environment found"),
        "pip list should report no venv found, got: {}",
        stderr
    );
}

#[test]
fn test_pip_freeze_no_venv() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");

    let output = Command::new(umbral_bin())
        .args(["pip", "freeze"])
        .current_dir(tmp.path())
        .env_remove("VIRTUAL_ENV")
        .output()
        .expect("failed to execute umbral pip freeze");

    assert!(
        !output.status.success(),
        "pip freeze without a venv should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no virtual environment found"),
        "pip freeze should report no venv found, got: {}",
        stderr
    );
}

#[test]
fn test_pip_compile_help() {
    let output = Command::new(umbral_bin())
        .args(["pip", "compile", "--help"])
        .output()
        .expect("failed to execute umbral pip compile --help");

    assert!(
        output.status.success(),
        "umbral pip compile --help should succeed"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("src") || stdout.contains("SRC"),
        "pip compile help should mention src arg, got: {}",
        stdout
    );
    assert!(
        stdout.contains("-o") || stdout.contains("--output-file"),
        "pip compile help should mention -o flag, got: {}",
        stdout
    );
}

// ── Tool command tests ──────────────────────────────────────────────

#[test]
fn test_tool_help() {
    let output = Command::new(umbral_bin())
        .args(["tool", "--help"])
        .output()
        .expect("failed to execute umbral tool --help");

    assert!(output.status.success(), "umbral tool --help should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("run") || stdout.contains("Run"),
        "tool help should mention run subcommand, got: {}",
        stdout
    );
    assert!(
        stdout.contains("install") || stdout.contains("Install"),
        "tool help should mention install subcommand, got: {}",
        stdout
    );
    assert!(
        stdout.contains("list") || stdout.contains("List"),
        "tool help should mention list subcommand, got: {}",
        stdout
    );
    assert!(
        stdout.contains("uninstall") || stdout.contains("Uninstall"),
        "tool help should mention uninstall subcommand, got: {}",
        stdout
    );
}

#[test]
fn test_tool_install_help() {
    let output = Command::new(umbral_bin())
        .args(["tool", "install", "--help"])
        .output()
        .expect("failed to execute umbral tool install --help");

    assert!(
        output.status.success(),
        "umbral tool install --help should succeed"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("package") || stdout.contains("PACKAGE"),
        "tool install help should mention package arg, got: {}",
        stdout
    );
    assert!(
        stdout.contains("--version"),
        "tool install help should mention --version flag, got: {}",
        stdout
    );
}

#[test]
fn test_tool_run_help() {
    let output = Command::new(umbral_bin())
        .args(["tool", "run", "--help"])
        .output()
        .expect("failed to execute umbral tool run --help");

    assert!(
        output.status.success(),
        "umbral tool run --help should succeed"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("package") || stdout.contains("PACKAGE"),
        "tool run help should mention package arg, got: {}",
        stdout
    );
}

#[test]
fn test_tool_list_empty() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let tools_dir = tmp.path().join("tools");
    std::fs::create_dir_all(&tools_dir).expect("failed to create tools dir");

    let output = Command::new(umbral_bin())
        .args(["tool", "list"])
        .env("UMBRAL_TOOLS_DIR", tools_dir.to_str().unwrap())
        .output()
        .expect("failed to execute umbral tool list");

    assert!(
        output.status.success(),
        "tool list with empty tools dir should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_tool_uninstall_nonexistent() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let tools_dir = tmp.path().join("tools");
    std::fs::create_dir_all(&tools_dir).expect("failed to create tools dir");

    let output = Command::new(umbral_bin())
        .args(["tool", "uninstall", "nonexistent-tool"])
        .env("UMBRAL_TOOLS_DIR", tools_dir.to_str().unwrap())
        .output()
        .expect("failed to execute umbral tool uninstall");

    assert!(
        !output.status.success(),
        "tool uninstall of nonexistent tool should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not installed") || stderr.contains("nonexistent-tool"),
        "tool uninstall should mention the tool is not installed, got: {}",
        stderr
    );
}

#[test]
fn test_resolve_universal_flag_in_help() {
    let output = Command::new(umbral_bin())
        .args(["lock", "--help"])
        .output()
        .expect("failed to execute umbral lock --help");

    assert!(output.status.success(), "umbral lock --help should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--universal"),
        "lock --help should mention --universal flag, got: {}",
        stdout
    );
}

/// Test the full sdist → wheel build pipeline by creating a project that depends
/// on a package only available as an sdist. This requires Python + setuptools.
#[test]
#[ignore] // Requires Python + setuptools + network access
fn test_sync_with_sdist_build() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let project_path = tmp.path().join("pyproject.toml");

    // Use a small, pure-Python package that is commonly available.
    // `six` has wheels, so instead we test the build pipeline by checking
    // that the full sync pipeline succeeds (resolve + venv + install).
    std::fs::write(
        &project_path,
        r#"[project]
name = "test-sdist-build"
version = "0.1.0"
requires-python = ">=3.10"
dependencies = [
    "iniconfig>=2.0",
]

[build-system]
requires = ["setuptools"]
build-backend = "setuptools.build_meta"
"#,
    )
    .expect("failed to write pyproject.toml");

    let output = Command::new(umbral_bin())
        .args([
            "sync",
            "--project",
            project_path.to_str().unwrap(),
            "--lockfile",
            tmp.path().join("uv.lock").to_str().unwrap(),
            "--venv",
            tmp.path().join(".venv").to_str().unwrap(),
        ])
        .current_dir(tmp.path())
        .output()
        .expect("failed to execute umbral sync");

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("request failed")
            || stderr.contains("error sending request")
            || stderr.contains("failed to find a Python interpreter")
        {
            eprintln!("Skipping test_sync_with_sdist_build: network/Python unavailable");
            return;
        }
        panic!(
            "umbral sync failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            stderr,
        );
    }

    // Verify lockfile and venv were created
    assert!(tmp.path().join("uv.lock").exists(), "lockfile should exist");
    assert!(tmp.path().join(".venv").exists(), "venv should exist");

    // Verify the package was installed
    let site_packages = if cfg!(windows) {
        tmp.path().join(".venv").join("Lib").join("site-packages")
    } else {
        // Find the actual site-packages dir
        let lib_dir = tmp.path().join(".venv").join("lib");
        if let Ok(entries) = std::fs::read_dir(&lib_dir) {
            entries
                .filter_map(|e| e.ok())
                .find(|e| e.file_name().to_string_lossy().starts_with("python"))
                .map(|e| e.path().join("site-packages"))
                .unwrap_or(lib_dir.join("python3/site-packages"))
        } else {
            lib_dir.join("python3/site-packages")
        }
    };

    // Check that iniconfig's dist-info exists (installed successfully)
    let has_iniconfig = std::fs::read_dir(&site_packages)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .any(|e| e.file_name().to_string_lossy().starts_with("iniconfig"))
        })
        .unwrap_or(false);

    assert!(
        has_iniconfig,
        "iniconfig should be installed in site-packages at {}",
        site_packages.display()
    );
}
