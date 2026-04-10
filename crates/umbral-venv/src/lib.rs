//! Pure-Rust virtual environment creation and Python interpreter discovery.
//!
//! Creates virtual environments without depending on `python -m venv`,
//! achieving ~4ms creation time vs ~1.5s for CPython's venv module.

pub mod python_download;

use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::symlink;

#[cfg(windows)]
fn symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(src, dst)
}
use std::process::Command;

use thiserror::Error;
use tracing::info;

use umbral_pep440::Version;

// ── Errors ──────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum VenvError {
    #[error("no Python interpreter found matching {0}")]
    NoPython(String),

    #[error("failed to run Python: {0}")]
    PythonExec(#[from] std::io::Error),

    #[error("failed to parse Python version output: {0}")]
    ParseVersion(String),

    #[error("failed to create venv at {path}: {source}")]
    Create {
        path: String,
        source: std::io::Error,
    },
}

// ── Python interpreter discovery ────────────────────────────────────

/// Information about a discovered Python interpreter.
#[derive(Debug, Clone)]
pub struct PythonInterpreter {
    /// Absolute path to the interpreter binary.
    pub path: PathBuf,
    /// Parsed version (e.g., 3.12.3).
    pub version: Version,
    /// Major.minor string (e.g., "3.12").
    pub major_minor: String,
    /// The sys.prefix path.
    pub prefix: PathBuf,
}

impl PythonInterpreter {
    /// Find a Python interpreter, optionally matching a version request.
    ///
    /// `version_request` can be `None` (any python3), `"3.12"`, `"3"`, etc.
    ///
    /// Search order:
    /// 1. `.python-version` file (if no explicit version_request given)
    /// 2. System Python interpreters on `$PATH`
    /// 3. Managed Python installations in `$UMBRAL_PYTHON_DIR` or
    ///    `~/.local/share/umbral/python/`
    pub fn find(version_request: Option<&str>) -> Result<Self, VenvError> {
        // If no explicit version requested, check .python-version file
        let file_version = if version_request.is_none() {
            read_python_version_file()
        } else {
            None
        };
        let version_request = version_request.or(file_version.as_deref());

        // First, try system Python interpreters
        let candidates = python_candidates(version_request);

        for candidate in &candidates {
            if let Ok(interp) = probe_interpreter(candidate) {
                if let Some(req) = version_request {
                    let matches = if req.contains('.') {
                        // Full major.minor match: "3.12" == "3.12"
                        interp.major_minor == req || interp.version.to_string() == req
                    } else {
                        // Major-only match: "3" matches "3.12" but NOT "31.0"
                        interp.major_minor.starts_with(req)
                            && interp.major_minor[req.len()..].starts_with('.')
                    };
                    if matches {
                        return Ok(interp);
                    }
                } else {
                    return Ok(interp);
                }
            }
        }

        // Second, check managed installations
        if let Some(req) = version_request {
            let install_dir = python_download::default_install_dir();
            if let Some(managed) = python_download::find_installed(&install_dir, req) {
                // Probe the managed interpreter to get full info
                let exe_str = managed.executable.to_string_lossy().to_string();
                if let Ok(interp) = probe_interpreter(&exe_str) {
                    return Ok(interp);
                }
            }
        }

        Err(VenvError::NoPython(
            version_request.unwrap_or("python3").to_string(),
        ))
    }

    /// The site-packages directory relative to the venv root.
    pub fn site_packages_rel(&self) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from("Lib").join("site-packages")
        } else {
            PathBuf::from(format!("lib/python{}/site-packages", self.major_minor))
        }
    }

    /// The bin directory name for this platform ("bin" on Unix, "Scripts" on Windows).
    pub fn bin_dir_name() -> &'static str {
        if cfg!(windows) {
            "Scripts"
        } else {
            "bin"
        }
    }
}

/// Read a `.python-version` file from the current directory or ancestors.
///
/// The file should contain a version string like "3.12" or "3.12.3" on the first
/// non-blank, non-comment line. This is compatible with pyenv and uv's behavior.
fn read_python_version_file() -> Option<String> {
    let start = std::env::current_dir().ok()?;
    read_python_version_file_from(&start)
}

/// Read a `.python-version` file starting from `start_dir` and walking up to ancestors.
fn read_python_version_file_from(start_dir: &Path) -> Option<String> {
    let mut dir = start_dir.to_path_buf();
    loop {
        let candidate = dir.join(".python-version");
        if candidate.is_file() {
            if let Ok(content) = std::fs::read_to_string(&candidate) {
                let version = content
                    .lines()
                    .map(|l| l.trim())
                    .find(|l| !l.is_empty() && !l.starts_with('#'))?;
                info!("using Python {} from {}", version, candidate.display());
                return Some(version.to_string());
            }
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

/// Generate a list of Python executable candidates to try.
fn python_candidates(version_request: Option<&str>) -> Vec<String> {
    let mut candidates = Vec::new();
    if let Some(req) = version_request {
        candidates.push(format!("python{req}"));
    }
    // Try specific versions in descending order
    for minor in (8..=15).rev() {
        candidates.push(format!("python3.{minor}"));
    }
    candidates.push("python3".to_string());
    candidates.push("python".to_string());
    candidates
}

/// Probe a Python executable to get its version and prefix.
fn probe_interpreter(name: &str) -> Result<PythonInterpreter, VenvError> {
    let output = Command::new(name)
        .args([
            "-c",
            "import sys; v=sys.version_info; print(f'{v.major}.{v.minor}.{v.micro}'); print(sys.prefix); print(sys.executable)",
        ])
        .output()?;

    if !output.status.success() {
        return Err(VenvError::ParseVersion(format!(
            "{name} exited with {}",
            output.status
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.trim().lines().collect();
    if lines.len() < 3 {
        return Err(VenvError::ParseVersion(format!(
            "unexpected output from {name}: {stdout}"
        )));
    }

    let version: Version = lines[0]
        .parse()
        .map_err(|_| VenvError::ParseVersion(lines[0].to_string()))?;

    let major_minor = format!("{}.{}", version.major(), version.minor());

    Ok(PythonInterpreter {
        path: PathBuf::from(lines[2].trim()),
        version,
        major_minor,
        prefix: PathBuf::from(lines[1].trim()),
    })
}

// ── Venv creation ───────────────────────────────────────────────────

/// Information about a created virtual environment.
#[derive(Debug, Clone)]
pub struct VenvInfo {
    /// Root directory of the venv.
    pub path: PathBuf,
    /// The Python interpreter used.
    pub interpreter: PythonInterpreter,
    /// Path to the venv's site-packages.
    pub site_packages: PathBuf,
    /// Path to the venv's bin directory.
    pub bin_dir: PathBuf,
}

/// Create a virtual environment at the given path.
///
/// This is a pure-Rust implementation that does not call `python -m venv`.
/// It creates the directory structure, symlinks the Python binary, and
/// writes `pyvenv.cfg` and activation scripts.
pub fn create_venv(
    path: &Path,
    interpreter: &PythonInterpreter,
    prompt: Option<&str>,
) -> Result<VenvInfo, VenvError> {
    // Create the root directory FIRST so canonicalize() can resolve it to an
    // absolute path.  Without this, canonicalize() fails on a non-existent
    // path and we fall back to a potentially-relative path.
    std::fs::create_dir_all(path).map_err(|e| VenvError::Create {
        path: path.display().to_string(),
        source: e,
    })?;
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    let bin_dir = path.join(PythonInterpreter::bin_dir_name());
    let (lib_dir, site_packages) = if cfg!(windows) {
        let lib = path.join("Lib");
        let sp = lib.join("site-packages");
        (lib, sp)
    } else {
        let lib = path
            .join("lib")
            .join(format!("python{}", interpreter.major_minor));
        let sp = lib.join("site-packages");
        (lib, sp)
    };
    let _ = &lib_dir; // suppress unused warning
    let include_dir = path.join("include");

    // Create directory structure
    for dir in [&bin_dir, &site_packages, &include_dir] {
        std::fs::create_dir_all(dir).map_err(|e| VenvError::Create {
            path: dir.display().to_string(),
            source: e,
        })?;
    }

    info!(
        path = %path.display(),
        python = %interpreter.path.display(),
        version = %interpreter.version,
        "creating virtual environment"
    );

    // Symlink (Unix) or copy (Windows) the Python binary
    #[cfg(unix)]
    {
        let python_link = bin_dir.join("python");
        if python_link.exists() || python_link.read_link().is_ok() {
            let _ = std::fs::remove_file(&python_link);
        }
        symlink(&interpreter.path, &python_link).map_err(|e| VenvError::Create {
            path: python_link.display().to_string(),
            source: e,
        })?;

        // Create additional symlinks: python3, python3.X
        // Use read_link() instead of exists() to detect dangling symlinks too.
        let python3_link = bin_dir.join("python3");
        if python3_link.read_link().is_ok() {
            let _ = std::fs::remove_file(&python3_link);
        }
        let _ = symlink(&interpreter.path, &python3_link);

        let pythonxy_link = bin_dir.join(format!("python{}", interpreter.major_minor));
        if pythonxy_link.read_link().is_ok() {
            let _ = std::fs::remove_file(&pythonxy_link);
        }
        let _ = symlink(&interpreter.path, &pythonxy_link);
    }

    #[cfg(windows)]
    {
        let python_exe = bin_dir.join("python.exe");
        if python_exe.exists() {
            let _ = std::fs::remove_file(&python_exe);
        }
        std::fs::copy(&interpreter.path, &python_exe).map_err(|e| VenvError::Create {
            path: python_exe.display().to_string(),
            source: e,
        })?;

        // Copy as python3.exe and pythonX.Y.exe too
        let python3_exe = bin_dir.join("python3.exe");
        if !python3_exe.exists() {
            let _ = std::fs::copy(&interpreter.path, &python3_exe);
        }
        let pythonxy_exe = bin_dir.join(format!("python{}.exe", interpreter.major_minor));
        if !pythonxy_exe.exists() {
            let _ = std::fs::copy(&interpreter.path, &pythonxy_exe);
        }
    }

    // Write pyvenv.cfg
    let home_dir = interpreter.path.parent().unwrap_or(Path::new("/usr/bin"));
    let prompt_str = prompt.unwrap_or("umbral");
    let pyvenv_cfg = format!(
        "home = {home}\ninclude-system-site-packages = false\nversion = {version}\nprompt = ({prompt})\n",
        home = home_dir.display(),
        version = interpreter.version,
        prompt = prompt_str,
    );
    write_file(&path.join("pyvenv.cfg"), &pyvenv_cfg)?;

    // Write activation scripts (generate all scripts on all platforms for
    // portability, matching CPython's behaviour).
    //
    // Design note on bin directory names in activation scripts:
    // - Bash (`activate`) and fish (`activate.fish`) always use `bin` because
    //   these shells only run on Unix or WSL, where the bin dir is `bin`.
    // - CMD (`activate.bat`) and PowerShell (`Activate.ps1`) always use
    //   `Scripts` because these are Windows-only shells where the bin dir is
    //   `Scripts`.
    write_activate_script(&bin_dir, &path, prompt_str)?;
    write_activate_fish(&bin_dir, &path, prompt_str)?;
    write_activate_bat(&bin_dir, &path, prompt_str)?;
    write_activate_ps1(&bin_dir, &path, prompt_str)?;

    Ok(VenvInfo {
        path: path.clone(),
        interpreter: interpreter.clone(),
        site_packages,
        bin_dir,
    })
}

/// Check if a directory looks like a valid venv.
pub fn is_venv(path: &Path) -> bool {
    if !path.join("pyvenv.cfg").exists() {
        return false;
    }
    if cfg!(windows) {
        path.join("Scripts").join("python.exe").exists()
    } else {
        path.join("bin").join("python").exists()
    }
}

/// Get the site-packages path for an existing venv.
pub fn venv_site_packages(path: &Path) -> Option<PathBuf> {
    // On Windows, site-packages lives under Lib/site-packages
    if cfg!(windows) {
        let sp = path.join("Lib").join("site-packages");
        if sp.exists() {
            return Some(sp);
        }
    }

    // On Unix, site-packages lives under lib/pythonX.Y/site-packages
    let lib_dir = path.join("lib");
    if let Ok(entries) = std::fs::read_dir(&lib_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.to_string_lossy().starts_with("python") {
                let sp = entry.path().join("site-packages");
                if sp.exists() {
                    return Some(sp);
                }
            }
        }
    }
    None
}

// ── File writers ────────────────────────────────────────────────────

fn write_file(path: &Path, content: &str) -> Result<(), VenvError> {
    let mut f = std::fs::File::create(path).map_err(|e| VenvError::Create {
        path: path.display().to_string(),
        source: e,
    })?;
    f.write_all(content.as_bytes())
        .map_err(|e| VenvError::Create {
            path: path.display().to_string(),
            source: e,
        })
}

fn write_activate_script(bin_dir: &Path, venv_path: &Path, prompt: &str) -> Result<(), VenvError> {
    let content = format!(
        r#"# This file must be sourced: . activate
deactivate () {{
    if [ -n "${{_OLD_VIRTUAL_PATH:-}}" ] ; then
        PATH="${{_OLD_VIRTUAL_PATH:-}}"
        export PATH
        unset _OLD_VIRTUAL_PATH
    fi
    if [ -n "${{_OLD_VIRTUAL_PS1:-}}" ] ; then
        PS1="${{_OLD_VIRTUAL_PS1:-}}"
        export PS1
        unset _OLD_VIRTUAL_PS1
    fi
    unset VIRTUAL_ENV
    unset VIRTUAL_ENV_PROMPT
    if [ ! "${{1:-}}" = "nondestructive" ] ; then
        unset -f deactivate
    fi
}}
deactivate nondestructive

VIRTUAL_ENV="{venv}"
export VIRTUAL_ENV

VIRTUAL_ENV_PROMPT="{prompt}"
export VIRTUAL_ENV_PROMPT

_OLD_VIRTUAL_PATH="$PATH"
PATH="$VIRTUAL_ENV/bin:$PATH"
export PATH

_OLD_VIRTUAL_PS1="${{PS1:-}}"
PS1="({prompt}) ${{PS1:-}}"
export PS1
"#,
        venv = venv_path.display(),
        prompt = prompt,
    );
    write_file(&bin_dir.join("activate"), &content)?;

    // Make activate executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            bin_dir.join("activate"),
            std::fs::Permissions::from_mode(0o755),
        );
    }
    Ok(())
}

fn write_activate_fish(bin_dir: &Path, venv_path: &Path, prompt: &str) -> Result<(), VenvError> {
    let content = format!(
        r#"function deactivate -d "Exit virtual environment"
    if test -n "$_OLD_VIRTUAL_PATH"
        set -gx PATH $_OLD_VIRTUAL_PATH
        set -e _OLD_VIRTUAL_PATH
    end
    if test -n "$_OLD_FISH_PROMPT_OVERRIDE"
        set -e _OLD_FISH_PROMPT_OVERRIDE
        functions -e fish_prompt
        if functions -q _old_fish_prompt
            functions -c _old_fish_prompt fish_prompt
            functions -e _old_fish_prompt
        end
    end
    set -e VIRTUAL_ENV
    set -e VIRTUAL_ENV_PROMPT
end

deactivate

set -gx VIRTUAL_ENV "{venv}"
set -gx VIRTUAL_ENV_PROMPT "{prompt}"
set -gx _OLD_VIRTUAL_PATH $PATH
set -gx PATH "$VIRTUAL_ENV/bin" $PATH
"#,
        venv = venv_path.display(),
        prompt = prompt,
    );
    write_file(&bin_dir.join("activate.fish"), &content)
}

fn write_activate_bat(bin_dir: &Path, venv_path: &Path, prompt: &str) -> Result<(), VenvError> {
    let content = format!(
        r#"@echo off
rem This file is generated by umbral for Windows CMD activation.

if defined _OLD_VIRTUAL_PROMPT (
    set "PROMPT=%_OLD_VIRTUAL_PROMPT%"
)
if defined _OLD_VIRTUAL_PATH (
    set "PATH=%_OLD_VIRTUAL_PATH%"
)

set "VIRTUAL_ENV={venv}"

if not defined PROMPT set PROMPT=$P$G

set "_OLD_VIRTUAL_PROMPT=%PROMPT%"
set "PROMPT=({prompt}) %PROMPT%"

set "_OLD_VIRTUAL_PATH=%PATH%"
set "PATH=%VIRTUAL_ENV%\Scripts;%PATH%"
"#,
        venv = venv_path.display(),
        prompt = prompt,
    );
    write_file(&bin_dir.join("activate.bat"), &content)
}

fn write_activate_ps1(bin_dir: &Path, venv_path: &Path, prompt: &str) -> Result<(), VenvError> {
    let content = format!(
        r#"# This file is generated by umbral for PowerShell activation.
function global:deactivate ([switch]$NonDestructive) {{
    if (Test-Path -Path variable:_OLD_VIRTUAL_PATH) {{
        $env:PATH = $variable:_OLD_VIRTUAL_PATH
        Remove-Variable -Name _OLD_VIRTUAL_PATH -Scope global
    }}
    if (Test-Path -Path function:_old_virtual_prompt) {{
        Copy-Item -Path function:_old_virtual_prompt -Destination function:prompt
        Remove-Item -Path function:_old_virtual_prompt
    }}
    if (Test-Path -Path env:VIRTUAL_ENV) {{
        Remove-Item env:VIRTUAL_ENV
    }}
    if (Test-Path -Path env:VIRTUAL_ENV_PROMPT) {{
        Remove-Item env:VIRTUAL_ENV_PROMPT
    }}
    if (-not $NonDestructive) {{
        Remove-Item -Path function:deactivate
    }}
}}

deactivate -NonDestructive

$env:VIRTUAL_ENV = "{venv}"
$env:VIRTUAL_ENV_PROMPT = "{prompt}"

$global:_OLD_VIRTUAL_PATH = $env:PATH
$env:PATH = "$env:VIRTUAL_ENV\Scripts" + [System.IO.Path]::PathSeparator + $env:PATH

Copy-Item -Path function:prompt -Destination function:_old_virtual_prompt
function global:prompt {{
    Write-Host -NoNewline -ForegroundColor Green "({prompt}) "
    _old_virtual_prompt
}}
"#,
        venv = venv_path.display(),
        prompt = prompt,
    );
    write_file(&bin_dir.join("Activate.ps1"), &content)
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_python3() {
        // This test requires python3 to be installed
        let interp = PythonInterpreter::find(None);
        if let Ok(interp) = interp {
            assert!(interp.path.exists());
            assert!(interp.major_minor.starts_with("3."));
            assert!(interp.version.major() == 3);
        }
        // If python3 not found, skip (CI may not have it)
    }

    #[test]
    fn test_python_candidates() {
        let candidates = python_candidates(Some("3.12"));
        assert_eq!(candidates[0], "python3.12");
        assert!(candidates.contains(&"python3".to_string()));
    }

    #[test]
    fn test_python_candidates_none() {
        let candidates = python_candidates(None);
        assert!(candidates.contains(&"python3".to_string()));
        assert!(!candidates[0].starts_with("python3.") || candidates.len() > 1);
    }

    #[test]
    fn test_create_venv() {
        let interp = match PythonInterpreter::find(None) {
            Ok(i) => i,
            Err(_) => return, // skip if no python
        };

        let tmp = tempfile::tempdir().unwrap();
        let venv_path = tmp.path().join(".venv");

        let info = create_venv(&venv_path, &interp, Some("test-project")).unwrap();

        // Check directory structure
        assert!(info.bin_dir.exists());
        assert!(info.site_packages.exists());
        assert!(venv_path.join("include").exists());

        // Check Python binary exists
        if cfg!(windows) {
            assert!(info.bin_dir.join("python.exe").exists());
        } else {
            assert!(info.bin_dir.join("python").exists());
            assert!(info.bin_dir.join("python3").exists());
        }

        // Check pyvenv.cfg
        let cfg = std::fs::read_to_string(venv_path.join("pyvenv.cfg")).unwrap();
        assert!(cfg.contains("include-system-site-packages = false"));
        assert!(cfg.contains(&format!("version = {}", interp.version)));
        assert!(cfg.contains("prompt = (test-project)"));

        if cfg!(windows) {
            // Check PowerShell activation script
            assert!(info.bin_dir.join("Activate.ps1").exists());
        } else {
            // Check activation scripts
            let activate = std::fs::read_to_string(info.bin_dir.join("activate")).unwrap();
            assert!(activate.contains("VIRTUAL_ENV="));
            assert!(activate.contains("(test-project)"));
            assert!(info.bin_dir.join("activate.fish").exists());
        }
    }

    #[test]
    fn test_is_venv() {
        let interp = match PythonInterpreter::find(None) {
            Ok(i) => i,
            Err(_) => return,
        };

        let tmp = tempfile::tempdir().unwrap();
        let venv_path = tmp.path().join(".venv");

        assert!(!is_venv(&venv_path));
        create_venv(&venv_path, &interp, None).unwrap();
        assert!(is_venv(&venv_path));
    }

    #[test]
    fn test_venv_site_packages() {
        let interp = match PythonInterpreter::find(None) {
            Ok(i) => i,
            Err(_) => return,
        };

        let tmp = tempfile::tempdir().unwrap();
        let venv_path = tmp.path().join(".venv");
        create_venv(&venv_path, &interp, None).unwrap();

        let sp = venv_site_packages(&venv_path).unwrap();
        assert!(sp.exists());
        assert!(sp.to_string_lossy().contains("site-packages"));
    }

    #[test]
    fn test_site_packages_rel() {
        let interp = PythonInterpreter {
            path: PathBuf::from("/usr/bin/python3.12"),
            version: "3.12.3".parse().unwrap(),
            major_minor: "3.12".to_string(),
            prefix: PathBuf::from("/usr"),
        };
        let rel = interp.site_packages_rel();
        if cfg!(windows) {
            assert_eq!(rel, PathBuf::from("Lib").join("site-packages"));
        } else {
            assert_eq!(rel, PathBuf::from("lib/python3.12/site-packages"));
        }
    }

    // ── Version matching tests ──────────────────────────────────────

    /// Helper: simulate version matching logic from `PythonInterpreter::find`
    /// without needing real Python binaries on disk.
    fn version_matches(major_minor: &str, req: &str) -> bool {
        if req.contains('.') {
            major_minor == req
        } else {
            major_minor.starts_with(req) && major_minor[req.len()..].starts_with('.')
        }
    }

    #[test]
    fn test_version_request_major_only_matches_3_12() {
        // "3" should match "3.12"
        assert!(version_matches("3.12", "3"));
    }

    #[test]
    fn test_version_request_major_only_matches_3_8() {
        // "3" should match "3.8"
        assert!(version_matches("3.8", "3"));
    }

    #[test]
    fn test_version_request_major_only_does_not_match_31() {
        // "3" must NOT match "31.0" (false prefix match)
        assert!(!version_matches("31.0", "3"));
    }

    #[test]
    fn test_version_request_full_match() {
        // "3.12" should match "3.12"
        assert!(version_matches("3.12", "3.12"));
    }

    #[test]
    fn test_version_request_full_mismatch() {
        // "3.12" should NOT match "3.11"
        assert!(!version_matches("3.11", "3.12"));
    }

    #[test]
    fn test_version_request_major_only_integration() {
        // Integration test: find with "3" should succeed if any Python 3 is installed
        let interp = PythonInterpreter::find(Some("3"));
        if let Ok(interp) = interp {
            assert!(interp.major_minor.starts_with("3."));
            assert_eq!(interp.version.major(), 3);
        }
        // If no python3, skip
    }

    #[cfg(unix)]
    #[test]
    fn test_recreate_venv_over_dangling_symlinks() {
        // Regression: re-creating a venv when python3 / python3.X symlinks
        // are dangling (target removed) should succeed, not fail with EEXIST.
        let interp = match PythonInterpreter::find(None) {
            Ok(i) => i,
            Err(_) => return, // skip if no python
        };

        let tmp = tempfile::tempdir().unwrap();
        let venv_path = tmp.path().join(".venv");

        // First creation — establishes symlinks
        create_venv(&venv_path, &interp, Some("test")).unwrap();

        // Sabotage: replace symlinks with dangling ones
        let bin = venv_path.join("bin");
        for name in ["python3", &format!("python{}", interp.major_minor)] {
            let link = bin.join(name);
            let _ = std::fs::remove_file(&link);
            symlink(Path::new("/nonexistent/python"), &link).unwrap();
            // Confirm it is dangling (exists() returns false, read_link() succeeds)
            assert!(!link.exists(), "link should be dangling");
            assert!(link.read_link().is_ok(), "link should still be a symlink");
        }

        // Second creation over the dangling symlinks should succeed
        let info = create_venv(&venv_path, &interp, Some("test")).unwrap();
        assert!(info.bin_dir.join("python3").exists());
        assert!(info
            .bin_dir
            .join(format!("python{}", interp.major_minor))
            .exists());
    }

    #[test]
    fn test_read_python_version_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".python-version"), "3.11\n").unwrap();

        let result = read_python_version_file_from(tmp.path());
        assert_eq!(result, Some("3.11".to_string()));
    }

    #[test]
    fn test_read_python_version_file_with_comments() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(".python-version"),
            "# managed by uv\n3.12.3\n",
        )
        .unwrap();

        let result = read_python_version_file_from(tmp.path());
        assert_eq!(result, Some("3.12.3".to_string()));
    }

    #[test]
    fn test_read_python_version_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let result = read_python_version_file_from(tmp.path());
        assert_eq!(result, None);
    }

    #[test]
    fn test_read_python_version_file_in_parent() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".python-version"), "3.10\n").unwrap();

        let subdir = tmp.path().join("src");
        std::fs::create_dir(&subdir).unwrap();

        let result = read_python_version_file_from(&subdir);
        assert_eq!(result, Some("3.10".to_string()));
    }
}
