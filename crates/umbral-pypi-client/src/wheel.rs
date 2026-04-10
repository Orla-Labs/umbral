use crate::error::{PypiClientError, Result};
use crate::tags::WheelTag;

/// Parsed components of a wheel filename.
///
/// Wheel filenames follow the pattern:
/// `{name}-{version}(-{build})?-{python}-{abi}-{platform}.whl`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WheelFilename {
    pub name: String,
    pub version: String,
    pub python_tag: String,
    pub abi_tag: String,
    pub platform_tag: String,
}

impl WheelFilename {
    /// Parse a wheel filename into its components.
    ///
    /// Parses from the right since the last 3 dash-separated segments are always
    /// `{python}-{abi}-{platform}`. This correctly handles distribution names that
    /// contain hyphens (non-compliant per PEP 427, but found in the wild).
    pub fn parse(filename: &str) -> Result<Self> {
        let stem = filename.strip_suffix(".whl").ok_or_else(|| {
            PypiClientError::InvalidWheelFilename(format!("not a .whl file: {filename}"))
        })?;

        // Pop the 3 required trailing tags from the right
        let (rest, platform_tag) = stem.rsplit_once('-').ok_or_else(|| {
            PypiClientError::InvalidWheelFilename(format!("invalid wheel filename: {filename}"))
        })?;
        let (rest, abi_tag) = rest.rsplit_once('-').ok_or_else(|| {
            PypiClientError::InvalidWheelFilename(format!("invalid wheel filename: {filename}"))
        })?;
        let (name_ver_build, python_tag) = rest.rsplit_once('-').ok_or_else(|| {
            PypiClientError::InvalidWheelFilename(format!("invalid wheel filename: {filename}"))
        })?;

        // Remaining: "{name}-{version}" or "{name}-{version}-{build}"
        // Build tags are numeric (no dots) per PEP 427, versions typically contain dots.
        let name_ver = if let Some((prefix, maybe_build)) = name_ver_build.rsplit_once('-') {
            if maybe_build.starts_with(|c: char| c.is_ascii_digit())
                && !maybe_build.contains('.')
                && prefix.contains('-')
            {
                // Looks like a build tag and there's still a name-version in prefix
                prefix
            } else {
                name_ver_build
            }
        } else {
            // No dashes — only a name with no version
            return Err(PypiClientError::InvalidWheelFilename(format!(
                "missing version in wheel filename: {filename}"
            )));
        };

        let (name, version) = name_ver.rsplit_once('-').ok_or_else(|| {
            PypiClientError::InvalidWheelFilename(format!(
                "missing version in wheel filename: {filename}"
            ))
        })?;

        if name.is_empty() {
            return Err(PypiClientError::InvalidWheelFilename(format!(
                "empty distribution name: {filename}"
            )));
        }

        Ok(WheelFilename {
            name: name.to_string(),
            version: version.to_string(),
            python_tag: python_tag.to_string(),
            abi_tag: abi_tag.to_string(),
            platform_tag: platform_tag.to_string(),
        })
    }

    /// Get all tags this wheel supports by expanding compressed tag forms.
    ///
    /// Wheel filenames can use dot-separated "compressed" tags. For example:
    /// `cp38.cp39` in the python tag means the wheel supports both cp38 and cp39.
    /// This method computes the cartesian product of all python, abi, and platform
    /// tag components.
    ///
    /// Example:
    /// ```text
    /// python_tag: "cp38.cp39"
    /// abi_tag: "cp38.cp39"
    /// platform_tag: "manylinux1_x86_64.manylinux_2_5_x86_64"
    /// ```
    /// produces 2 * 2 * 2 = 8 individual tag triples.
    pub fn tags(&self) -> Vec<WheelTag> {
        let pythons: Vec<&str> = self.python_tag.split('.').collect();
        let abis: Vec<&str> = self.abi_tag.split('.').collect();
        let platforms: Vec<&str> = self.platform_tag.split('.').collect();

        let mut tags = Vec::with_capacity(pythons.len() * abis.len() * platforms.len());

        for python in &pythons {
            for abi in &abis {
                for platform in &platforms {
                    tags.push(WheelTag {
                        python: python.to_string(),
                        abi: abi.to_string(),
                        platform: platform.to_string(),
                    });
                }
            }
        }

        tags
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_wheel() {
        let whl = WheelFilename::parse("requests-2.31.0-py3-none-any.whl").unwrap();
        assert_eq!(whl.name, "requests");
        assert_eq!(whl.version, "2.31.0");
        assert_eq!(whl.python_tag, "py3");
        assert_eq!(whl.abi_tag, "none");
        assert_eq!(whl.platform_tag, "any");
    }

    #[test]
    fn test_parse_native_wheel() {
        let whl =
            WheelFilename::parse("numpy-1.26.0-cp312-cp312-manylinux_2_17_x86_64.whl").unwrap();
        assert_eq!(whl.name, "numpy");
        assert_eq!(whl.version, "1.26.0");
        assert_eq!(whl.python_tag, "cp312");
        assert_eq!(whl.abi_tag, "cp312");
        assert_eq!(whl.platform_tag, "manylinux_2_17_x86_64");
    }

    #[test]
    fn test_parse_wheel_with_build_tag() {
        let whl = WheelFilename::parse("package-1.0.0-1-cp312-cp312-linux_x86_64.whl").unwrap();
        assert_eq!(whl.name, "package");
        assert_eq!(whl.version, "1.0.0");
        assert_eq!(whl.python_tag, "cp312");
        assert_eq!(whl.abi_tag, "cp312");
        assert_eq!(whl.platform_tag, "linux_x86_64");
    }

    #[test]
    fn test_reject_non_whl() {
        assert!(WheelFilename::parse("requests-2.31.0.tar.gz").is_err());
    }

    #[test]
    fn test_reject_too_few_parts() {
        assert!(WheelFilename::parse("requests-2.31.0-py3.whl").is_err());
    }

    #[test]
    fn test_tags_simple() {
        let whl = WheelFilename::parse("requests-2.31.0-py3-none-any.whl").unwrap();
        let tags = whl.tags();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].python, "py3");
        assert_eq!(tags[0].abi, "none");
        assert_eq!(tags[0].platform, "any");
    }

    #[test]
    fn test_tags_native() {
        let whl =
            WheelFilename::parse("numpy-1.26.0-cp312-cp312-manylinux_2_17_x86_64.whl").unwrap();
        let tags = whl.tags();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].python, "cp312");
        assert_eq!(tags[0].abi, "cp312");
        assert_eq!(tags[0].platform, "manylinux_2_17_x86_64");
    }

    #[test]
    fn test_tags_compressed_python() {
        let whl = WheelFilename::parse(
            "package-1.0.0-cp310.cp311.cp312-cp310.cp311.cp312-manylinux_2_17_x86_64.whl",
        )
        .unwrap();
        let tags = whl.tags();
        // 3 python * 3 abi * 1 platform = 9
        assert_eq!(tags.len(), 9);
        // Verify a specific combination exists
        assert!(tags.iter().any(|t| {
            t.python == "cp312" && t.abi == "cp312" && t.platform == "manylinux_2_17_x86_64"
        }));
        assert!(tags.iter().any(|t| {
            t.python == "cp310" && t.abi == "cp311" && t.platform == "manylinux_2_17_x86_64"
        }));
    }

    #[test]
    fn test_tags_compressed_platform() {
        let whl = WheelFilename::parse(
            "package-1.0.0-cp312-cp312-manylinux1_x86_64.manylinux_2_5_x86_64.whl",
        )
        .unwrap();
        let tags = whl.tags();
        // 1 python * 1 abi * 2 platforms = 2
        assert_eq!(tags.len(), 2);
        assert!(tags.iter().any(|t| t.platform == "manylinux1_x86_64"));
        assert!(tags.iter().any(|t| t.platform == "manylinux_2_5_x86_64"));
    }

    #[test]
    fn test_tags_fully_compressed() {
        let whl = WheelFilename::parse(
            "package-1.0.0-cp311.cp312-abi3.none-manylinux_2_17_x86_64.linux_x86_64.whl",
        )
        .unwrap();
        let tags = whl.tags();
        // 2 python * 2 abi * 2 platform = 8
        assert_eq!(tags.len(), 8);
    }

    #[test]
    fn test_parse_hyphenated_name() {
        // Non-compliant per PEP 427 (should use underscores), but exists in the wild
        let whl = WheelFilename::parse("python-xlib-0.35-py3-none-any.whl").unwrap();
        assert_eq!(whl.name, "python-xlib");
        assert_eq!(whl.version, "0.35");
        assert_eq!(whl.python_tag, "py3");
        assert_eq!(whl.abi_tag, "none");
        assert_eq!(whl.platform_tag, "any");
    }

    #[test]
    fn test_parse_multi_hyphenated_name() {
        let whl =
            WheelFilename::parse("my-cool-package-2.1.0-cp312-cp312-linux_x86_64.whl").unwrap();
        assert_eq!(whl.name, "my-cool-package");
        assert_eq!(whl.version, "2.1.0");
    }

    #[test]
    fn test_parse_hyphenated_name_with_build_tag() {
        let whl = WheelFilename::parse("my-package-1.0.0-1-cp312-cp312-linux_x86_64.whl").unwrap();
        assert_eq!(whl.name, "my-package");
        assert_eq!(whl.version, "1.0.0");
    }

    #[test]
    fn test_parse_underscore_name() {
        // Compliant PEP 427 form
        let whl = WheelFilename::parse("python_dateutil-2.8.2-py2.py3-none-any.whl").unwrap();
        assert_eq!(whl.name, "python_dateutil");
        assert_eq!(whl.version, "2.8.2");
    }
}
