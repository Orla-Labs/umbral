//! In-memory mock package registry for testing the resolver without network access.
//!
//! # Example
//!
//! ```rust,no_run
//! use umbral_resolver::mock::MockRegistry;
//!
//! let mut registry = MockRegistry::new();
//! registry
//!     .add_version("numpy", "1.26.0", vec![])
//!     .add_version("numpy", "2.0.0", vec![])
//!     .add_version("pandas", "2.0.0", vec![("numpy", ">=1.22")]);
//! ```

use std::collections::HashMap;

use umbral_pep440::{PackageName, Version, VersionSpecifiers};
use umbral_pep508::Requirement;

use crate::provider::{PackageMetadata, PackageSource};

/// A mock package registry backed by in-memory data.
#[derive(Debug, Default, Clone)]
pub struct MockRegistry {
    packages: HashMap<String, Vec<MockPackageVersion>>,
    /// Packages to report as sdist-only (for testing SdistOnly hint injection).
    sdist_only: std::collections::HashSet<String>,
}

/// A single version entry in the mock registry.
#[derive(Debug, Clone)]
pub struct MockPackageVersion {
    pub version: Version,
    pub dependencies: Vec<(String, String)>,
    pub requires_python: Option<String>,
    pub yanked: bool,
    pub extras: HashMap<String, Vec<(String, String)>>,
    /// Raw PEP 508 requirement strings for dependencies that include markers.
    /// When non-empty, these override `dependencies` in `get_metadata`.
    pub raw_requirements: Vec<String>,
    /// If true, `get_metadata` returns `None` (simulates unavailable metadata).
    pub metadata_unavailable: bool,
}

impl MockRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a package version with dependencies.
    /// Dependencies are `(name, version_specifier)` pairs (e.g., `("numpy", ">=1.0")`).
    pub fn add_version(&mut self, name: &str, version: &str, deps: Vec<(&str, &str)>) -> &mut Self {
        self.packages
            .entry(name.to_string())
            .or_default()
            .push(MockPackageVersion {
                version: version.parse().expect("invalid version"),
                dependencies: deps
                    .into_iter()
                    .map(|(n, v)| (n.to_string(), v.to_string()))
                    .collect(),
                requires_python: None,
                yanked: false,
                extras: HashMap::new(),
                raw_requirements: vec![],
                metadata_unavailable: false,
            });
        self
    }

    /// Add a package version with a `requires-python` constraint.
    pub fn add_version_with_python(
        &mut self,
        name: &str,
        version: &str,
        deps: Vec<(&str, &str)>,
        requires_python: &str,
    ) -> &mut Self {
        self.packages
            .entry(name.to_string())
            .or_default()
            .push(MockPackageVersion {
                version: version.parse().expect("invalid version"),
                dependencies: deps
                    .into_iter()
                    .map(|(n, v)| (n.to_string(), v.to_string()))
                    .collect(),
                requires_python: Some(requires_python.to_string()),
                yanked: false,
                extras: HashMap::new(),
                raw_requirements: vec![],
                metadata_unavailable: false,
            });
        self
    }

    /// Add a yanked package version.
    pub fn add_yanked_version(
        &mut self,
        name: &str,
        version: &str,
        deps: Vec<(&str, &str)>,
    ) -> &mut Self {
        self.packages
            .entry(name.to_string())
            .or_default()
            .push(MockPackageVersion {
                version: version.parse().expect("invalid version"),
                dependencies: deps
                    .into_iter()
                    .map(|(n, v)| (n.to_string(), v.to_string()))
                    .collect(),
                requires_python: None,
                yanked: true,
                extras: HashMap::new(),
                raw_requirements: vec![],
                metadata_unavailable: false,
            });
        self
    }

    /// Add a package version with extras.
    /// `extras` maps extra names to their additional dependencies.
    pub fn add_version_with_extras(
        &mut self,
        name: &str,
        version: &str,
        deps: Vec<(&str, &str)>,
        extras: HashMap<String, Vec<(&str, &str)>>,
    ) -> &mut Self {
        self.packages
            .entry(name.to_string())
            .or_default()
            .push(MockPackageVersion {
                version: version.parse().expect("invalid version"),
                dependencies: deps
                    .into_iter()
                    .map(|(n, v)| (n.to_string(), v.to_string()))
                    .collect(),
                requires_python: None,
                yanked: false,
                extras: extras
                    .into_iter()
                    .map(|(k, v)| {
                        (
                            k,
                            v.into_iter()
                                .map(|(n, vs)| (n.to_string(), vs.to_string()))
                                .collect(),
                        )
                    })
                    .collect(),
                raw_requirements: vec![],
                metadata_unavailable: false,
            });
        self
    }

    /// Add a package version with raw PEP 508 requirement strings that may
    /// include marker expressions (e.g., `"numpy>=1.0; python_version >= \"3.8\""`).
    pub fn add_version_with_raw_deps(
        &mut self,
        name: &str,
        version: &str,
        raw_deps: Vec<&str>,
    ) -> &mut Self {
        self.packages
            .entry(name.to_string())
            .or_default()
            .push(MockPackageVersion {
                version: version.parse().expect("invalid version"),
                dependencies: vec![],
                requires_python: None,
                yanked: false,
                extras: HashMap::new(),
                raw_requirements: raw_deps.into_iter().map(|s| s.to_string()).collect(),
                metadata_unavailable: false,
            });
        self
    }

    /// Mark a package as sdist-only (has files on the index but zero wheels).
    /// This causes `sdist_only_packages()` to include it, for testing hint injection.
    pub fn mark_sdist_only(&mut self, name: &str) -> &mut Self {
        self.sdist_only.insert(name.to_string());
        self
    }

    /// Add a package version whose metadata is unavailable (get_metadata returns None).
    pub fn add_version_no_metadata(&mut self, name: &str, version: &str) -> &mut Self {
        self.packages
            .entry(name.to_string())
            .or_default()
            .push(MockPackageVersion {
                version: version.parse().expect("invalid version"),
                dependencies: vec![],
                requires_python: None,
                yanked: false,
                extras: HashMap::new(),
                raw_requirements: vec![],
                metadata_unavailable: true,
            });
        self
    }
}

impl PackageSource for MockRegistry {
    fn available_versions(&self, package: &PackageName) -> Vec<Version> {
        let name = package.as_str();
        let mut versions: Vec<Version> = self
            .packages
            .get(name)
            .map(|versions| {
                versions
                    .iter()
                    .filter(|v| !v.yanked)
                    .map(|v| v.version.clone())
                    .collect()
            })
            .unwrap_or_default();

        // Sort descending (newest first) — this is the preference order
        versions.sort();
        versions.reverse();
        versions
    }

    fn sdist_only_packages(&self) -> std::collections::HashSet<String> {
        self.sdist_only.clone()
    }

    fn get_metadata(&self, package: &PackageName, version: &Version) -> Option<PackageMetadata> {
        let name = package.as_str();
        let pkg_versions = self.packages.get(name)?;
        let mock_version = pkg_versions.iter().find(|v| &v.version == version)?;

        // Simulate unavailable metadata.
        if mock_version.metadata_unavailable {
            return None;
        }

        // If raw_requirements are provided, parse them as full PEP 508 strings
        // (supports markers). Otherwise fall back to the simple (name, spec) pairs.
        let dependencies = if !mock_version.raw_requirements.is_empty() {
            mock_version
                .raw_requirements
                .iter()
                .map(|s| Requirement::parse(s).expect("invalid raw requirement"))
                .collect()
        } else {
            mock_version
                .dependencies
                .iter()
                .map(|(name, spec)| Requirement {
                    name: PackageName::new(name),
                    extras: vec![],
                    version: if spec.is_empty() {
                        None
                    } else {
                        Some(
                            spec.parse::<VersionSpecifiers>()
                                .expect("invalid specifier"),
                        )
                    },
                    url: None,
                    marker: None,
                })
                .collect()
        };

        let requires_python = mock_version.requires_python.as_ref().map(|s| {
            s.parse::<VersionSpecifiers>()
                .expect("invalid python specifier")
        });

        let extras = mock_version
            .extras
            .iter()
            .map(|(extra, deps)| {
                let extra_deps = deps
                    .iter()
                    .map(|(name, spec)| Requirement {
                        name: PackageName::new(name),
                        extras: vec![],
                        version: if spec.is_empty() {
                            None
                        } else {
                            Some(
                                spec.parse::<VersionSpecifiers>()
                                    .expect("invalid specifier"),
                            )
                        },
                        url: None,
                        marker: None,
                    })
                    .collect();
                (extra.clone(), extra_deps)
            })
            .collect();

        Some(PackageMetadata {
            dependencies,
            requires_python,
            yanked: mock_version.yanked,
            extras,
        })
    }
}
