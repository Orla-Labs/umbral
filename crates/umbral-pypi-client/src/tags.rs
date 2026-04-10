//! Platform tag detection for wheel compatibility.
//!
//! Detects the current platform's compatible wheel tags by probing the Python
//! interpreter and the OS environment. Tags are returned in priority order
//! (most specific first) so that callers can select the best matching wheel.
//!
//! Wheel filenames follow the pattern:
//! `{name}-{version}(-{build})?-{python}-{abi}-{platform}.whl`
//!
//! References:
//! - PEP 425: Compatibility Tags for Built Distributions
//! - PEP 600: Future "manylinux" Platform Tags

use std::path::Path;
use std::process::Command;

use crate::error::{PypiClientError, Result};
use crate::wheel::WheelFilename;

/// A single compatibility tag triple.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WheelTag {
    pub python: String,
    pub abi: String,
    pub platform: String,
}

impl WheelTag {
    /// Format this tag as a canonical string (e.g., `cp312-cp312-manylinux_2_17_x86_64`).
    pub fn as_tag_string(&self) -> String {
        format!("{}-{}-{}", self.python, self.abi, self.platform)
    }
}

/// The set of compatible wheel tags for the current platform, ordered by
/// preference (most specific / highest priority first).
#[derive(Debug, Clone)]
pub struct PlatformTags {
    /// Compatible tags in priority order (most specific first).
    pub tags: Vec<WheelTag>,
}

impl PlatformTags {
    /// Detect compatible tags by probing the Python interpreter at `python_path`.
    ///
    /// Runs a small Python snippet to extract version info, platform, and
    /// architecture, then enumerates all compatible tag triples.
    pub fn detect(python_path: &Path) -> Result<Self> {
        let info = probe_python(python_path)?;
        let tags = build_tag_list(&info);
        Ok(PlatformTags { tags })
    }

    /// Build a `PlatformTags` from explicit interpreter info (for testing).
    #[cfg(test)]
    pub fn from_info(info: &PythonInfo) -> Self {
        let tags = build_tag_list(info);
        PlatformTags { tags }
    }

    /// Check if a wheel filename is compatible with this platform.
    pub fn is_compatible(&self, filename: &str) -> bool {
        self.compatibility_score(filename).is_some()
    }

    /// Score a wheel filename for compatibility. Lower scores are more preferred.
    /// Returns `None` if the wheel is not compatible at all.
    ///
    /// The score is the index of the first matching tag in the priority-ordered
    /// tag list. This means the most specific/preferred tags get the lowest scores.
    pub fn compatibility_score(&self, filename: &str) -> Option<usize> {
        let wf = match WheelFilename::parse(filename) {
            Ok(wf) => wf,
            Err(_) => return None,
        };

        self.compatibility_score_for_wheel(&wf)
    }

    /// Score a parsed wheel filename for compatibility.
    pub fn compatibility_score_for_wheel(&self, wf: &WheelFilename) -> Option<usize> {
        let wheel_tags = wf.tags();
        let mut best_score: Option<usize> = None;

        for wt in &wheel_tags {
            for (i, pt) in self.tags.iter().enumerate() {
                if wt.python == pt.python && wt.abi == pt.abi && wt.platform == pt.platform {
                    match best_score {
                        Some(current) if i < current => best_score = Some(i),
                        None => best_score = Some(i),
                        _ => {}
                    }
                }
            }
        }

        best_score
    }
}

/// Information extracted from a Python interpreter.
#[derive(Debug, Clone)]
pub struct PythonInfo {
    /// Major version (e.g., 3)
    pub major: u32,
    /// Minor version (e.g., 12)
    pub minor: u32,
    /// Platform string from `sys.platform` (e.g., "linux", "darwin", "win32")
    pub sys_platform: String,
    /// Machine architecture from `platform.machine()` (e.g., "x86_64", "arm64", "aarch64")
    pub machine: String,
    /// Pointer size in bytes (4 or 8)
    pub pointer_size: u32,
    /// macOS deployment target version if on macOS (major, minor)
    pub macos_version: Option<(u32, u32)>,
    /// Whether the system uses musl libc (e.g., Alpine Linux)
    pub is_musl: bool,
    /// musl libc version if detected (major, minor), e.g., (1, 2)
    pub musl_version: Option<(u32, u32)>,
}

/// Probe a Python interpreter to extract platform information.
fn probe_python(python_path: &Path) -> Result<PythonInfo> {
    let script = r#"
import sys, struct, platform
v = sys.version_info
mac_ver = platform.mac_ver()[0]
parts = [
    str(v.major),
    str(v.minor),
    sys.platform,
    platform.machine(),
    str(struct.calcsize('P')),
    mac_ver if mac_ver else '',
]
print('\n'.join(parts))
"#;

    let output = Command::new(python_path)
        .arg("-c")
        .arg(script)
        .output()
        .map_err(|e| {
            PypiClientError::MetadataParse(format!(
                "failed to run Python interpreter at {}: {}",
                python_path.display(),
                e
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(PypiClientError::MetadataParse(format!(
            "Python interpreter probe failed: {}",
            stderr.trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.trim().lines().collect();

    if lines.len() < 5 {
        return Err(PypiClientError::MetadataParse(format!(
            "unexpected Python probe output (expected 5+ lines, got {}): {:?}",
            lines.len(),
            stdout.trim()
        )));
    }

    let major: u32 = lines[0].parse().map_err(|e| {
        PypiClientError::MetadataParse(format!("failed to parse Python major version: {}", e))
    })?;
    let minor: u32 = lines[1].parse().map_err(|e| {
        PypiClientError::MetadataParse(format!("failed to parse Python minor version: {}", e))
    })?;
    let sys_platform = lines[2].to_string();
    let machine = lines[3].to_string();
    let pointer_size: u32 = lines[4].parse().map_err(|e| {
        PypiClientError::MetadataParse(format!("failed to parse pointer size: {}", e))
    })?;

    let macos_version = if lines.len() > 5 && !lines[5].is_empty() {
        parse_macos_version(lines[5])
    } else {
        None
    };

    // Detect musl libc on Linux systems
    let (is_musl, musl_version) = if sys_platform == "linux" {
        detect_musl()
    } else {
        (false, None)
    };

    Ok(PythonInfo {
        major,
        minor,
        sys_platform,
        machine,
        pointer_size,
        macos_version,
        is_musl,
        musl_version,
    })
}

/// Parse a macOS version string like "14.2.1" into (major, minor).
fn parse_macos_version(version_str: &str) -> Option<(u32, u32)> {
    let parts: Vec<&str> = version_str.split('.').collect();
    if parts.len() >= 2 {
        let major = parts[0].parse().ok()?;
        let minor = parts[1].parse().ok()?;
        Some((major, minor))
    } else if parts.len() == 1 {
        let major = parts[0].parse().ok()?;
        Some((major, 0))
    } else {
        None
    }
}

/// Build the full list of compatible tags in priority order.
///
/// The ordering follows pip's conventions:
/// 1. CPython-specific tags with native ABI (most specific)
/// 2. CPython-specific tags with abi3 (stable ABI)
/// 3. CPython-specific tags with none ABI
/// 4. Generic py3 tags with none ABI
/// 5. Platform-specific to "any" platform (least specific)
fn build_tag_list(info: &PythonInfo) -> Vec<WheelTag> {
    let mut tags = Vec::new();

    let python_tags = python_tags(info);
    let _abi_tags = abi_tags(info);
    let platform_tags = platform_tags(info);

    // 1. Most specific: cpXY with cpXY ABI on specific platforms
    let cp_tag = format!("cp{}{}", info.major, info.minor);
    for platform in &platform_tags {
        // CPython native ABI
        tags.push(WheelTag {
            python: cp_tag.clone(),
            abi: cp_tag.clone(),
            platform: platform.clone(),
        });
    }

    // 2. abi3 tags: cpXY with abi3 on specific platforms (for stable ABI)
    // Enumerate from current version down to cp32
    for minor in (2..=info.minor).rev() {
        let cp = format!("cp{}{}", info.major, minor);
        for platform in &platform_tags {
            tags.push(WheelTag {
                python: cp.clone(),
                abi: "abi3".to_string(),
                platform: platform.clone(),
            });
        }
    }

    // 3. cpXY with none ABI on specific platforms
    for platform in &platform_tags {
        tags.push(WheelTag {
            python: cp_tag.clone(),
            abi: "none".to_string(),
            platform: platform.clone(),
        });
    }

    // 4. Generic python tags (py3, py3X) with none ABI on specific platforms
    for py_tag in &python_tags {
        if py_tag == &cp_tag {
            continue; // Already covered above
        }
        for platform in &platform_tags {
            tags.push(WheelTag {
                python: py_tag.clone(),
                abi: "none".to_string(),
                platform: platform.clone(),
            });
        }
    }

    // 5. All python tags with none ABI on "any" platform
    for py_tag in &python_tags {
        tags.push(WheelTag {
            python: py_tag.clone(),
            abi: "none".to_string(),
            platform: "any".to_string(),
        });
    }

    tags
}

/// Generate Python tags in priority order.
///
/// For CPython 3.12: `["cp312", "cp311", ..., "cp3", "py312", "py311", ..., "py3", "py2.py3"]`
fn python_tags(info: &PythonInfo) -> Vec<String> {
    let mut tags = Vec::new();

    // CPython-specific tags, current version first then downward
    for minor in (0..=info.minor).rev() {
        tags.push(format!("cp{}{}", info.major, minor));
    }

    // Generic pyXY tags
    for minor in (0..=info.minor).rev() {
        tags.push(format!("py{}{}", info.major, minor));
    }

    // Generic pyX tag
    tags.push(format!("py{}", info.major));

    tags
}

/// Generate ABI tags in priority order.
///
/// For CPython 3.12: `["cp312", "abi3", "none"]`
fn abi_tags(info: &PythonInfo) -> Vec<String> {
    vec![
        format!("cp{}{}", info.major, info.minor),
        "abi3".to_string(),
        "none".to_string(),
    ]
}

/// Generate platform tags in priority order.
fn platform_tags(info: &PythonInfo) -> Vec<String> {
    let mut tags = Vec::new();

    match info.sys_platform.as_str() {
        "darwin" => {
            tags.extend(macos_platform_tags(info));
        }
        "linux" => {
            tags.extend(linux_platform_tags(info));
        }
        "win32" => {
            tags.extend(windows_platform_tags(info));
        }
        _ => {
            // Unknown platform — only "any" will match (added by caller)
        }
    }

    tags
}

/// Generate macOS platform tags.
///
/// For ARM Macs: `macosx_{major}_{minor}_arm64`, `macosx_{major}_{minor}_universal2`
/// For Intel Macs: `macosx_{major}_{minor}_x86_64`, `macosx_{major}_{minor}_universal2`, `macosx_{major}_{minor}_intel`
///
/// Versions are enumerated downward from the current version to 10_9 (the minimum
/// version that pip supports).
fn macos_platform_tags(info: &PythonInfo) -> Vec<String> {
    let mut tags = Vec::new();

    let (cur_major, cur_minor) = info.macos_version.unwrap_or((10, 9));

    let arch = match info.machine.as_str() {
        "arm64" | "aarch64" => "arm64",
        _ => "x86_64",
    };

    // For major version >= 11, Apple simplified versioning to X_0
    // Enumerate from current down to 11_0 (for arm64) or 10_9 (for x86_64)
    if cur_major >= 11 {
        // For major versions >= 11, only minor version 0 is used
        for major in (11..=cur_major).rev() {
            let minor_start = if major == cur_major { cur_minor } else { 0 };
            for minor in (0..=minor_start).rev() {
                tags.push(format!("macosx_{}_{}_{}", major, minor, arch));
                tags.push(format!("macosx_{}_{}_{}", major, minor, "universal2"));
            }
        }

        // Also add 10_X tags for x86_64 (Rosetta compatibility) and universal2
        if arch == "x86_64" {
            for minor in (9..=16).rev() {
                tags.push(format!("macosx_10_{}_{}", minor, "x86_64"));
                tags.push(format!("macosx_10_{}_{}", minor, "universal2"));
                tags.push(format!("macosx_10_{}_{}", minor, "intel"));
            }
        } else {
            // arm64 can also use universal2 wheels built for 10_X (though rare)
            // but NOT x86_64 wheels — those require Rosetta and aren't wheel-compatible
            for minor in (9..=16).rev() {
                tags.push(format!("macosx_10_{}_{}", minor, "universal2"));
            }
        }
    } else {
        // macOS 10.X
        for minor in (9..=cur_minor).rev() {
            tags.push(format!("macosx_10_{}_{}", minor, arch));
            tags.push(format!("macosx_10_{}_{}", minor, "universal2"));
            if arch == "x86_64" {
                tags.push(format!("macosx_10_{}_{}", minor, "intel"));
            }
        }
    }

    tags
}

/// Generate Linux platform tags (manylinux, musllinux, and linux).
///
/// On musl-based systems, generates musllinux tags instead of manylinux
/// (musl and glibc are mutually exclusive). On glibc systems, enumerates
/// manylinux tags down to the 2_5 floor.
fn linux_platform_tags(info: &PythonInfo) -> Vec<String> {
    let mut tags = Vec::new();

    let arch = normalize_linux_arch(&info.machine);

    if info.is_musl {
        // Musl-based system: generate musllinux tags only (no manylinux)
        tags.extend(musllinux_platform_tags(info, arch));
    } else {
        // Glibc-based system: generate manylinux tags
        tags.extend(manylinux_platform_tags(arch));
    }

    // Fallback: native linux tag
    tags.push(format!("linux_{}", arch));

    tags
}

/// Generate manylinux platform tags for glibc-based systems.
///
/// Enumerates from the detected glibc version down to 2_5 (the lowest
/// manylinux floor), plus legacy aliases.
fn manylinux_platform_tags(arch: &str) -> Vec<String> {
    let mut tags = Vec::new();

    let glibc = detect_glibc_version().unwrap_or((2, 17));

    // Modern manylinux_X_Y tags (PEP 600), enumerate from current glibc down to 2_5
    for glibc_minor in (5..=glibc.1).rev() {
        tags.push(format!("manylinux_{}_{}_{}", glibc.0, glibc_minor, arch));
    }

    // Legacy manylinux aliases
    // manylinux2014 == manylinux_2_17
    if glibc.0 >= 2 && glibc.1 >= 17 {
        tags.push(format!("manylinux2014_{}", arch));
    }
    // manylinux2010 == manylinux_2_12
    if glibc.0 >= 2 && glibc.1 >= 12 {
        tags.push(format!("manylinux2010_{}", arch));
    }
    // manylinux1 == manylinux_2_5
    if glibc.0 >= 2 && glibc.1 >= 5 {
        tags.push(format!("manylinux1_{}", arch));
    }

    tags
}

/// Generate musllinux platform tags for musl-based systems.
///
/// Enumerates musllinux tags from the detected musl version down to 1_1.
/// For example, on musl 1.2: `musllinux_1_2_{arch}`, `musllinux_1_1_{arch}`.
fn musllinux_platform_tags(info: &PythonInfo, arch: &str) -> Vec<String> {
    let mut tags = Vec::new();

    let musl_version = info.musl_version.unwrap_or((1, 2));

    // Enumerate from detected musl version down to 1_1
    for musl_minor in (1..=musl_version.1).rev() {
        tags.push(format!(
            "musllinux_{}_{}_{}",
            musl_version.0, musl_minor, arch
        ));
    }

    tags
}

/// Generate Windows platform tags.
fn windows_platform_tags(info: &PythonInfo) -> Vec<String> {
    let mut tags = Vec::new();

    match info.machine.as_str() {
        "AMD64" | "x86_64" => {
            tags.push("win_amd64".to_string());
        }
        "x86" | "i686" | "i386" => {
            tags.push("win32".to_string());
        }
        "ARM64" | "aarch64" | "arm64" => {
            tags.push("win_arm64".to_string());
        }
        other => {
            // Fallback: use the machine name as-is
            tags.push(format!("win_{}", other.to_lowercase()));
        }
    }

    tags
}

/// Normalize Linux architecture names to match wheel tag conventions.
fn normalize_linux_arch(machine: &str) -> &str {
    match machine {
        "i386" | "i486" | "i586" | "i686" => "i686",
        "aarch64" => "aarch64",
        "arm64" => "aarch64",
        "x86_64" | "AMD64" => "x86_64",
        "armv7l" => "armv7l",
        "ppc64le" => "ppc64le",
        "s390x" => "s390x",
        other => other,
    }
}

/// Detect the system glibc version on Linux.
///
/// Tries parsing the output of `ldd --version`.
fn detect_glibc_version() -> Option<(u32, u32)> {
    let output = Command::new("ldd").arg("--version").output().ok()?;

    // ldd prints to stdout or stderr depending on implementation
    let text = if output.stdout.is_empty() {
        String::from_utf8_lossy(&output.stderr).to_string()
    } else {
        String::from_utf8_lossy(&output.stdout).to_string()
    };

    // Look for a pattern like "2.35" or "GLIBC 2.35"
    for line in text.lines() {
        // Common patterns:
        //   "ldd (GNU libc) 2.35"
        //   "ldd (Ubuntu GLIBC 2.35-0ubuntu3.6) 2.35"
        // We look for the last occurrence of X.Y where X and Y are numbers.
        if let Some(version) = extract_glibc_version_from_line(line) {
            return Some(version);
        }
    }

    None
}

/// Extract a glibc version (major, minor) from a single line of ldd output.
fn extract_glibc_version_from_line(line: &str) -> Option<(u32, u32)> {
    // Find patterns like "2.35" at the end of the line or after known prefixes
    let mut last_match: Option<(u32, u32)> = None;

    for word in line.split_whitespace() {
        let parts: Vec<&str> = word.split('.').collect();
        if parts.len() == 2 {
            if let (Ok(major), Ok(minor)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
                if major == 2 && minor > 0 {
                    last_match = Some((major, minor));
                }
            }
        }
    }

    last_match
}

/// Detect if the current system uses musl libc.
///
/// Checks for the musl dynamic linker at `/lib/ld-musl-*.so.1`. If found,
/// attempts to parse the musl version from `ldd --version` output. Falls back
/// to version 1.2 if detection succeeds but version parsing fails.
fn detect_musl() -> (bool, Option<(u32, u32)>) {
    // Check for musl dynamic linker files: /lib/ld-musl-*.so.1
    let musl_found = std::fs::read_dir("/lib")
        .map(|entries| {
            entries.filter_map(|e| e.ok()).any(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with("ld-musl-") && name.ends_with(".so.1")
            })
        })
        .unwrap_or(false);

    if !musl_found {
        return (false, None);
    }

    // Try to get the musl version from ldd --version.
    // On musl systems, ldd --version prints to stderr with a line like:
    //   "musl libc (x86_64)\nVersion 1.2.4"
    let version = Command::new("ldd")
        .arg("--version")
        .output()
        .ok()
        .and_then(|output| {
            // musl ldd writes to stderr (and exits non-zero)
            let text = if output.stderr.is_empty() {
                String::from_utf8_lossy(&output.stdout).to_string()
            } else {
                String::from_utf8_lossy(&output.stderr).to_string()
            };
            extract_musl_version(&text)
        })
        .unwrap_or((1, 2)); // Default to 1.2 if parsing fails

    (true, Some(version))
}

/// Extract musl version from ldd --version output.
///
/// Looks for a line like "Version 1.2.4" or "musl libc ... 1.2.4".
fn extract_musl_version(text: &str) -> Option<(u32, u32)> {
    for line in text.lines() {
        let line_lower = line.to_lowercase();
        if line_lower.contains("version") || line_lower.contains("musl") {
            // Look for a version pattern like "1.2" or "1.2.4"
            for word in line.split_whitespace() {
                let parts: Vec<&str> = word.split('.').collect();
                if parts.len() >= 2 {
                    if let (Ok(major), Ok(minor)) =
                        (parts[0].parse::<u32>(), parts[1].parse::<u32>())
                    {
                        if major == 1 && minor > 0 {
                            return Some((major, minor));
                        }
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_info_linux_x86() -> PythonInfo {
        PythonInfo {
            major: 3,
            minor: 12,
            sys_platform: "linux".to_string(),
            machine: "x86_64".to_string(),
            pointer_size: 8,
            macos_version: None,
            is_musl: false,
            musl_version: None,
        }
    }

    fn test_info_linux_musl_x86() -> PythonInfo {
        PythonInfo {
            major: 3,
            minor: 12,
            sys_platform: "linux".to_string(),
            machine: "x86_64".to_string(),
            pointer_size: 8,
            macos_version: None,
            is_musl: true,
            musl_version: Some((1, 2)),
        }
    }

    fn test_info_macos_arm64() -> PythonInfo {
        PythonInfo {
            major: 3,
            minor: 12,
            sys_platform: "darwin".to_string(),
            machine: "arm64".to_string(),
            pointer_size: 8,
            macos_version: Some((14, 0)),
            is_musl: false,
            musl_version: None,
        }
    }

    fn test_info_macos_x86() -> PythonInfo {
        PythonInfo {
            major: 3,
            minor: 12,
            sys_platform: "darwin".to_string(),
            machine: "x86_64".to_string(),
            pointer_size: 8,
            macos_version: Some((13, 0)),
            is_musl: false,
            musl_version: None,
        }
    }

    fn test_info_windows() -> PythonInfo {
        PythonInfo {
            major: 3,
            minor: 12,
            sys_platform: "win32".to_string(),
            machine: "AMD64".to_string(),
            pointer_size: 8,
            macos_version: None,
            is_musl: false,
            musl_version: None,
        }
    }

    #[test]
    fn test_tag_list_contains_native_cpython() {
        let info = test_info_linux_x86();
        let tags = PlatformTags::from_info(&info);

        // Should contain the native CPython tag
        assert!(
            tags.tags.iter().any(|t| {
                t.python == "cp312" && t.abi == "cp312" && t.platform.contains("x86_64")
            }),
            "should contain cp312-cp312-*x86_64 tag"
        );
    }

    #[test]
    fn test_tag_list_contains_abi3() {
        let info = test_info_linux_x86();
        let tags = PlatformTags::from_info(&info);

        assert!(
            tags.tags
                .iter()
                .any(|t| { t.python == "cp312" && t.abi == "abi3" }),
            "should contain cp312-abi3-* tags"
        );
    }

    #[test]
    fn test_tag_list_contains_pure_python() {
        let info = test_info_linux_x86();
        let tags = PlatformTags::from_info(&info);

        assert!(
            tags.tags
                .iter()
                .any(|t| { t.python == "py3" && t.abi == "none" && t.platform == "any" }),
            "should contain py3-none-any tag"
        );
    }

    #[test]
    fn test_tag_list_contains_py3x_none_any() {
        let info = test_info_linux_x86();
        let tags = PlatformTags::from_info(&info);

        assert!(
            tags.tags
                .iter()
                .any(|t| { t.python == "py312" && t.abi == "none" && t.platform == "any" }),
            "should contain py312-none-any tag"
        );
    }

    #[test]
    fn test_compatibility_native_beats_pure_python() {
        let info = test_info_linux_x86();
        let tags = PlatformTags::from_info(&info);

        let native = "numpy-1.26.0-cp312-cp312-manylinux_2_17_x86_64.whl";
        let pure = "requests-2.31.0-py3-none-any.whl";

        let native_score = tags.compatibility_score(native);
        let pure_score = tags.compatibility_score(pure);

        assert!(native_score.is_some(), "native wheel should be compatible");
        assert!(pure_score.is_some(), "pure wheel should be compatible");
        assert!(
            native_score.unwrap() < pure_score.unwrap(),
            "native wheel (score={}) should be preferred over pure python (score={})",
            native_score.unwrap(),
            pure_score.unwrap()
        );
    }

    #[test]
    fn test_is_compatible_rejects_wrong_platform() {
        let info = test_info_linux_x86();
        let tags = PlatformTags::from_info(&info);

        let macos_wheel = "numpy-1.26.0-cp312-cp312-macosx_14_0_arm64.whl";
        let win_wheel = "numpy-1.26.0-cp312-cp312-win_amd64.whl";

        assert!(
            !tags.is_compatible(macos_wheel),
            "macOS wheel should not be compatible on Linux"
        );
        assert!(
            !tags.is_compatible(win_wheel),
            "Windows wheel should not be compatible on Linux"
        );
    }

    #[test]
    fn test_is_compatible_rejects_wrong_python_version() {
        let info = test_info_linux_x86();
        let tags = PlatformTags::from_info(&info);

        // cp313 wheel should not be compatible with cp312 interpreter
        let future_wheel = "numpy-2.0.0-cp313-cp313-manylinux_2_17_x86_64.whl";
        assert!(
            !tags.is_compatible(future_wheel),
            "cp313 wheel should not be compatible with cp312 interpreter"
        );
    }

    #[test]
    fn test_is_compatible_accepts_abi3_wheel() {
        let info = test_info_linux_x86();
        let tags = PlatformTags::from_info(&info);

        let abi3_wheel = "cryptography-41.0.0-cp37-abi3-manylinux_2_17_x86_64.whl";
        assert!(
            tags.is_compatible(abi3_wheel),
            "abi3 wheel (cp37-abi3) should be compatible with cp312"
        );
    }

    #[test]
    fn test_pure_python_compatible_everywhere() {
        for info in &[
            test_info_linux_x86(),
            test_info_macos_arm64(),
            test_info_macos_x86(),
            test_info_windows(),
        ] {
            let tags = PlatformTags::from_info(info);
            let pure = "requests-2.31.0-py3-none-any.whl";
            assert!(
                tags.is_compatible(pure),
                "py3-none-any should be compatible on {:?}",
                info.sys_platform
            );
        }
    }

    #[test]
    fn test_macos_arm64_tags_contain_universal2() {
        let info = test_info_macos_arm64();
        let tags = PlatformTags::from_info(&info);

        assert!(
            tags.tags.iter().any(|t| t.platform.contains("universal2")),
            "macOS ARM64 tags should include universal2"
        );
    }

    #[test]
    fn test_macos_arm64_compatible_with_universal2_wheel() {
        let info = test_info_macos_arm64();
        let tags = PlatformTags::from_info(&info);

        let universal = "numpy-1.26.0-cp312-cp312-macosx_11_0_universal2.whl";
        assert!(
            tags.is_compatible(universal),
            "ARM64 Mac should be compatible with universal2 wheels"
        );
    }

    #[test]
    fn test_macos_version_enumeration() {
        let info = test_info_macos_arm64();
        let tags = PlatformTags::from_info(&info);

        // Should include tags for versions from 14_0 down to 11_0
        assert!(tags.tags.iter().any(|t| t.platform == "macosx_14_0_arm64"));
        assert!(tags.tags.iter().any(|t| t.platform == "macosx_13_0_arm64"));
        assert!(tags.tags.iter().any(|t| t.platform == "macosx_12_0_arm64"));
        assert!(tags.tags.iter().any(|t| t.platform == "macosx_11_0_arm64"));
    }

    #[test]
    fn test_windows_platform_tags() {
        let info = test_info_windows();
        let tags = PlatformTags::from_info(&info);

        assert!(
            tags.tags.iter().any(|t| t.platform == "win_amd64"),
            "Windows AMD64 tags should include win_amd64"
        );
    }

    #[test]
    fn test_compressed_wheel_tags_compatible() {
        // A wheel with compressed tags like cp38.cp39.cp310.cp311.cp312
        let info = test_info_linux_x86();
        let tags = PlatformTags::from_info(&info);

        // This wheel supports multiple Python versions
        let wheel = "package-1.0.0-cp310.cp311.cp312-cp310.cp311.cp312-manylinux_2_17_x86_64.whl";
        assert!(
            tags.is_compatible(wheel),
            "compressed tag wheel should be compatible"
        );
    }

    #[test]
    fn test_glibc_version_parsing() {
        assert_eq!(
            extract_glibc_version_from_line("ldd (GNU libc) 2.35"),
            Some((2, 35))
        );
        assert_eq!(
            extract_glibc_version_from_line("ldd (Ubuntu GLIBC 2.35-0ubuntu3.6) 2.35"),
            Some((2, 35))
        );
        assert_eq!(extract_glibc_version_from_line("no version here"), None);
    }

    #[test]
    fn test_detect_produces_valid_tags_for_current_platform() {
        // This test runs against the actual system, so tag content varies.
        // We just verify the structure is valid.
        let python = which_python();
        let Some(python_path) = python else {
            // Skip if no Python available in test environment
            eprintln!("skipping test_detect: no python3 found");
            return;
        };

        let tags = PlatformTags::detect(&python_path).expect("detect should succeed");

        assert!(!tags.tags.is_empty(), "should detect at least one tag");

        // Every tag should have non-empty fields
        for tag in &tags.tags {
            assert!(!tag.python.is_empty(), "python tag should not be empty");
            assert!(!tag.abi.is_empty(), "abi tag should not be empty");
            assert!(!tag.platform.is_empty(), "platform tag should not be empty");
        }

        // Should always include py3-none-any
        assert!(
            tags.tags
                .iter()
                .any(|t| { t.python == "py3" && t.abi == "none" && t.platform == "any" }),
            "should always include py3-none-any"
        );
    }

    #[test]
    fn test_macos_x86_includes_intel_tag() {
        let info = test_info_macos_x86();
        let tags = PlatformTags::from_info(&info);

        assert!(
            tags.tags.iter().any(|t| t.platform.contains("intel")),
            "macOS x86 should include intel platform tag for compatibility"
        );
    }

    // ── Edge case: Pure Python wheel compatible everywhere ────────

    #[test]
    fn test_pure_python_py3_none_any_compatible_all_platforms() {
        let pure_wheel = "some_package-1.0.0-py3-none-any.whl";
        let platforms = [
            test_info_linux_x86(),
            test_info_macos_arm64(),
            test_info_macos_x86(),
            test_info_windows(),
        ];
        for info in &platforms {
            let tags = PlatformTags::from_info(info);
            assert!(
                tags.is_compatible(pure_wheel),
                "py3-none-any should be compatible on {} / {}",
                info.sys_platform,
                info.machine
            );
        }
    }

    // ── Edge case: ABI3 wheel compatibility across Python versions ──

    #[test]
    fn test_abi3_cp38_compatible_with_cp312() {
        let info = test_info_linux_x86();
        let tags = PlatformTags::from_info(&info);

        // cp38-abi3 wheel should be compatible with cp312 (stable ABI)
        let abi3_wheel = "package-1.0.0-cp38-abi3-manylinux_2_17_x86_64.whl";
        assert!(
            tags.is_compatible(abi3_wheel),
            "cp38-abi3 wheel should be compatible with cp312 interpreter"
        );

        // cp312-abi3 should also be compatible (exact version match)
        let abi3_exact = "package-1.0.0-cp312-abi3-manylinux_2_17_x86_64.whl";
        assert!(
            tags.is_compatible(abi3_exact),
            "cp312-abi3 wheel should be compatible with cp312 interpreter"
        );

        // cp313-abi3 should NOT be compatible (future version)
        let abi3_future = "package-1.0.0-cp313-abi3-manylinux_2_17_x86_64.whl";
        assert!(
            !tags.is_compatible(abi3_future),
            "cp313-abi3 wheel should NOT be compatible with cp312 interpreter"
        );
    }

    // ── Edge case: ABI3 preference ordering ─────────────────────────

    #[test]
    fn test_abi3_scores_lower_for_higher_python() {
        // Among abi3 wheels, cp312-abi3 should score better (lower) than cp38-abi3
        // because the tag list has cp312-abi3 before cp38-abi3
        let info = test_info_linux_x86();
        let tags = PlatformTags::from_info(&info);

        let abi3_312 = "package-1.0.0-cp312-abi3-manylinux_2_17_x86_64.whl";
        let abi3_38 = "package-1.0.0-cp38-abi3-manylinux_2_17_x86_64.whl";

        let score_312 = tags.compatibility_score(abi3_312);
        let score_38 = tags.compatibility_score(abi3_38);

        assert!(score_312.is_some(), "cp312-abi3 should be compatible");
        assert!(score_38.is_some(), "cp38-abi3 should be compatible");
        assert!(
            score_312.unwrap() < score_38.unwrap(),
            "cp312-abi3 (score={}) should be preferred over cp38-abi3 (score={})",
            score_312.unwrap(),
            score_38.unwrap()
        );
    }

    // ── Musl detection tests ────────────────────────────────────────

    #[test]
    fn test_detect_musl_returns_false_on_non_musl_system() {
        // On a standard glibc system (or macOS/Windows), /lib/ld-musl-*.so.1
        // should not exist, so detect_musl returns (false, None).
        // On CI/local dev machines that ARE musl, this test still passes
        // because we test the function contract: if it returns true, version
        // must be Some.
        let (is_musl, version) = detect_musl();
        if is_musl {
            assert!(
                version.is_some(),
                "if musl is detected, version should be Some"
            );
        }
        // On non-musl systems (the common case), just verify no panic.
    }

    #[test]
    fn test_extract_musl_version_from_output() {
        // Typical musl ldd --version stderr output
        let output = "musl libc (x86_64)\nVersion 1.2.4\n";
        assert_eq!(extract_musl_version(output), Some((1, 2)));

        let output2 = "musl libc (aarch64)\nVersion 1.1.24\n";
        assert_eq!(extract_musl_version(output2), Some((1, 1)));

        let output3 = "not musl at all";
        assert_eq!(extract_musl_version(output3), None);
    }

    // ── Musllinux tag generation tests ─────────────────────────────

    #[test]
    fn test_musllinux_tags_generated_for_musl_system() {
        let info = test_info_linux_musl_x86();
        let tags = PlatformTags::from_info(&info);

        // Should contain musllinux_1_2_x86_64 and musllinux_1_1_x86_64
        assert!(
            tags.tags
                .iter()
                .any(|t| t.platform == "musllinux_1_2_x86_64"),
            "musl system should have musllinux_1_2_x86_64 tag"
        );
        assert!(
            tags.tags
                .iter()
                .any(|t| t.platform == "musllinux_1_1_x86_64"),
            "musl system should have musllinux_1_1_x86_64 tag"
        );
    }

    #[test]
    fn test_musllinux_no_manylinux_tags() {
        let info = test_info_linux_musl_x86();
        let tags = PlatformTags::from_info(&info);

        // Musl systems should NOT have any manylinux tags
        assert!(
            !tags.tags.iter().any(|t| t.platform.contains("manylinux")),
            "musl system should NOT have any manylinux tags"
        );
    }

    #[test]
    fn test_musllinux_has_linux_fallback() {
        let info = test_info_linux_musl_x86();
        let tags = PlatformTags::from_info(&info);

        assert!(
            tags.tags.iter().any(|t| t.platform == "linux_x86_64"),
            "musl system should still have native linux_x86_64 fallback tag"
        );
    }

    #[test]
    fn test_musllinux_wheel_compatible_on_musl() {
        let info = test_info_linux_musl_x86();
        let tags = PlatformTags::from_info(&info);

        let musl_wheel = "cryptography-42.0.0-cp312-cp312-musllinux_1_2_x86_64.whl";
        assert!(
            tags.is_compatible(musl_wheel),
            "musllinux wheel should be compatible on musl system"
        );

        let musl_wheel_old = "package-1.0.0-cp312-cp312-musllinux_1_1_x86_64.whl";
        assert!(
            tags.is_compatible(musl_wheel_old),
            "older musllinux_1_1 wheel should be compatible on musl 1.2 system"
        );
    }

    #[test]
    fn test_musllinux_wheel_incompatible_on_glibc() {
        let info = test_info_linux_x86();
        let tags = PlatformTags::from_info(&info);

        let musl_wheel = "cryptography-42.0.0-cp312-cp312-musllinux_1_2_x86_64.whl";
        assert!(
            !tags.is_compatible(musl_wheel),
            "musllinux wheel should NOT be compatible on glibc system"
        );
    }

    #[test]
    fn test_manylinux_wheel_incompatible_on_musl() {
        let info = test_info_linux_musl_x86();
        let tags = PlatformTags::from_info(&info);

        let manylinux_wheel = "numpy-1.26.0-cp312-cp312-manylinux_2_17_x86_64.whl";
        assert!(
            !tags.is_compatible(manylinux_wheel),
            "manylinux wheel should NOT be compatible on musl system"
        );
    }

    #[test]
    fn test_musllinux_prefers_newer_musl_version() {
        // On a musl 1.2 system, musllinux_1_2 should score better than musllinux_1_1
        let info = test_info_linux_musl_x86();
        let tags = PlatformTags::from_info(&info);

        let musl_12 = "package-1.0.0-cp312-cp312-musllinux_1_2_x86_64.whl";
        let musl_11 = "package-1.0.0-cp312-cp312-musllinux_1_1_x86_64.whl";

        let score_12 = tags.compatibility_score(musl_12);
        let score_11 = tags.compatibility_score(musl_11);

        assert!(score_12.is_some(), "musllinux_1_2 should be compatible");
        assert!(score_11.is_some(), "musllinux_1_1 should be compatible");
        assert!(
            score_12.unwrap() < score_11.unwrap(),
            "musllinux_1_2 (score={}) should be preferred over musllinux_1_1 (score={})",
            score_12.unwrap(),
            score_11.unwrap()
        );
    }

    // ── manylinux_2_5 floor tests ──────────────────────────────────

    #[test]
    fn test_manylinux_includes_2_5_floor() {
        let info = test_info_linux_x86();
        let tags = PlatformTags::from_info(&info);

        // With glibc >= 2.17 (the default), we should now see tags down to 2_5
        assert!(
            tags.tags
                .iter()
                .any(|t| t.platform == "manylinux_2_5_x86_64"),
            "glibc system should have manylinux_2_5_x86_64 tag"
        );
        assert!(
            tags.tags
                .iter()
                .any(|t| t.platform == "manylinux_2_12_x86_64"),
            "glibc system should have manylinux_2_12_x86_64 tag"
        );
        assert!(
            tags.tags
                .iter()
                .any(|t| t.platform == "manylinux_2_17_x86_64"),
            "glibc system should have manylinux_2_17_x86_64 tag"
        );
    }

    #[test]
    fn test_manylinux_legacy_aliases_present() {
        let info = test_info_linux_x86();
        let tags = PlatformTags::from_info(&info);

        assert!(
            tags.tags
                .iter()
                .any(|t| t.platform == "manylinux2014_x86_64"),
            "should have manylinux2014 alias (== 2_17)"
        );
        assert!(
            tags.tags
                .iter()
                .any(|t| t.platform == "manylinux2010_x86_64"),
            "should have manylinux2010 alias (== 2_12)"
        );
        assert!(
            tags.tags.iter().any(|t| t.platform == "manylinux1_x86_64"),
            "should have manylinux1 alias (== 2_5)"
        );
    }

    #[test]
    fn test_manylinux_2_5_wheel_compatible() {
        let info = test_info_linux_x86();
        let tags = PlatformTags::from_info(&info);

        let wheel_2_5 = "package-1.0.0-cp312-cp312-manylinux_2_5_x86_64.whl";
        assert!(
            tags.is_compatible(wheel_2_5),
            "manylinux_2_5 wheel should be compatible on glibc >= 2.17 system"
        );

        let wheel_manylinux1 = "package-1.0.0-cp312-cp312-manylinux1_x86_64.whl";
        assert!(
            tags.is_compatible(wheel_manylinux1),
            "manylinux1 wheel should be compatible on glibc >= 2.17 system"
        );
    }

    #[test]
    fn test_manylinux_prefers_higher_glibc() {
        // Newer manylinux (2_17) should score better than older (2_5)
        let info = test_info_linux_x86();
        let tags = PlatformTags::from_info(&info);

        let wheel_2_17 = "package-1.0.0-cp312-cp312-manylinux_2_17_x86_64.whl";
        let wheel_2_5 = "package-1.0.0-cp312-cp312-manylinux_2_5_x86_64.whl";

        let score_2_17 = tags.compatibility_score(wheel_2_17);
        let score_2_5 = tags.compatibility_score(wheel_2_5);

        assert!(score_2_17.is_some(), "manylinux_2_17 should be compatible");
        assert!(score_2_5.is_some(), "manylinux_2_5 should be compatible");
        assert!(
            score_2_17.unwrap() < score_2_5.unwrap(),
            "manylinux_2_17 (score={}) should be preferred over manylinux_2_5 (score={})",
            score_2_17.unwrap(),
            score_2_5.unwrap()
        );
    }

    /// Helper: find a python3 interpreter in PATH.
    fn which_python() -> Option<std::path::PathBuf> {
        let candidates = if cfg!(windows) {
            vec!["python", "python3"]
        } else {
            vec!["python3", "python"]
        };
        let which_cmd = if cfg!(windows) { "where" } else { "which" };
        for name in &candidates {
            if let Ok(output) = Command::new(which_cmd).arg(name).output() {
                if output.status.success() {
                    // `where` on Windows may return multiple lines; take the first
                    let path = String::from_utf8_lossy(&output.stdout)
                        .lines()
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if !path.is_empty() {
                        return Some(std::path::PathBuf::from(path));
                    }
                }
            }
        }
        None
    }
}
