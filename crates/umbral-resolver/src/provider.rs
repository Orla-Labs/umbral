//! The PubGrub `DependencyProvider` implementation for Umbral.
//!
//! `UmbralProvider` bridges between Python packaging concepts and PubGrub's
//! abstract dependency resolution algorithm.

use std::borrow::Borrow;
use std::collections::HashMap;
use std::error::Error;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use pubgrub::range::Range;
use pubgrub::solver::{Dependencies, DependencyProvider};
use pubgrub::type_aliases::Map;

use umbral_pep440::{PackageName, Version, VersionSpecifiers};
use umbral_pep508::{MarkerEnvironment, Requirement};

use crate::package::UmbralPackage;
use crate::range_conv::specifiers_to_range;
use crate::version::UmbralVersion;

/// Pre-release inclusion policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PreReleasePolicy {
    /// Never include pre-releases.
    #[default]
    Disallow,
    /// Always include pre-releases.
    Allow,
}

/// Configuration for the resolver.
#[derive(Debug, Clone)]
pub struct ResolverConfig {
    /// Target Python version (e.g., "3.11").
    pub python_version: Version,
    /// Target environment for marker evaluation.
    pub markers: Option<MarkerEnvironment>,
    /// Pre-release inclusion policy.
    pub pre_release_policy: PreReleasePolicy,
}

/// Metadata for a specific package version.
#[derive(Debug, Clone)]
pub struct PackageMetadata {
    pub dependencies: Vec<Requirement>,
    pub requires_python: Option<VersionSpecifiers>,
    pub yanked: bool,
    /// Extra name → additional dependencies for that extra.
    pub extras: HashMap<String, Vec<Requirement>>,
}

/// Trait for providing package information to the resolver.
///
/// Implement this for your package source (PyPI client, local cache, etc.).
/// For testing, use `MockRegistry` from the `mock` module.
pub trait PackageSource {
    /// List available versions for a package, in preference order (newest first).
    /// Yanked versions should be excluded by default.
    fn available_versions(&self, package: &PackageName) -> Vec<Version>;

    /// Get metadata for a specific package version.
    fn get_metadata(&self, package: &PackageName, version: &Version) -> Option<PackageMetadata>;

    /// Return the set of packages that are sdist-only (have files on the index
    /// but zero wheels). Used to produce better error messages when resolution
    /// fails. The default implementation returns an empty set.
    fn sdist_only_packages(&self) -> std::collections::HashSet<String> {
        std::collections::HashSet::new()
    }

    /// Return all distribution files (wheels + sdists) for a package version.
    /// Used to populate lockfile artifact URLs. Default returns empty.
    fn distribution_files(
        &self,
        _package: &PackageName,
        _version: &Version,
    ) -> Vec<DistributionFileInfo> {
        Vec::new()
    }
}

/// Artifact information for a distribution file (wheel or sdist).
#[derive(Debug, Clone)]
pub struct DistributionFileInfo {
    pub filename: String,
    pub url: String,
    pub hash: Option<String>,
    pub size: Option<u64>,
}

/// Maximum number of `choose_package_version` calls before the resolver
/// aborts. Prevents pathological dependency graphs from running forever.
const MAX_ITERATIONS: usize = 100_000;

/// The PubGrub dependency provider for Umbral.
pub struct UmbralProvider<S: PackageSource> {
    source: S,
    config: ResolverConfig,
    root_requirements: Vec<Requirement>,
    /// Override dependencies: when a dependency name matches a key in this map,
    /// the version specifier is completely replaced with the override value.
    /// This implements `[tool.uv] override-dependencies`.
    overrides: HashMap<PackageName, VersionSpecifiers>,
    /// Incremented by `should_cancel` each time PubGrub polls for
    /// cancellation. When it exceeds [`MAX_ITERATIONS`] the resolution
    /// is terminated with an error.
    iteration_count: AtomicUsize,
}

impl<S: PackageSource> UmbralProvider<S> {
    pub fn new(source: S, config: ResolverConfig, root_requirements: Vec<Requirement>) -> Self {
        Self {
            source,
            config,
            root_requirements,
            overrides: HashMap::new(),
            iteration_count: AtomicUsize::new(0),
        }
    }

    /// Create a provider with override dependencies.
    ///
    /// Overrides completely replace version specifiers: if a dependency name
    /// matches a key in `overrides`, the original version specifier from the
    /// package metadata is replaced with the override value.
    pub fn with_overrides(
        source: S,
        config: ResolverConfig,
        root_requirements: Vec<Requirement>,
        overrides: HashMap<PackageName, VersionSpecifiers>,
    ) -> Self {
        Self {
            source,
            config,
            root_requirements,
            overrides,
            iteration_count: AtomicUsize::new(0),
        }
    }

    pub fn source(&self) -> &S {
        &self.source
    }

    pub fn config(&self) -> &ResolverConfig {
        &self.config
    }

    /// Get available versions for a package, filtered by policy and range.
    fn get_versions(
        &self,
        package: &UmbralPackage,
        range: &Range<UmbralVersion>,
    ) -> Vec<UmbralVersion> {
        match package {
            UmbralPackage::Root => {
                let v = UmbralVersion::new(Version::new(vec![0]));
                if range.contains(&v) {
                    vec![v]
                } else {
                    vec![]
                }
            }
            UmbralPackage::Python => {
                let v = UmbralVersion::new(self.config.python_version.clone());
                if range.contains(&v) {
                    vec![v]
                } else {
                    vec![]
                }
            }
            UmbralPackage::Package(name) | UmbralPackage::Extra(name, _) => self
                .source
                .available_versions(name)
                .into_iter()
                .filter(|v| {
                    if v.is_prerelease()
                        && self.config.pre_release_policy == PreReleasePolicy::Disallow
                    {
                        return false;
                    }
                    true
                })
                .map(UmbralVersion::new)
                .filter(|v| range.contains(v))
                .collect(),
        }
    }

    /// Compute the version range for a dependency requirement, applying
    /// overrides if one exists for the dependency name. When an override is
    /// present, the original version specifier is completely replaced.
    fn range_for_dep(&self, req: &Requirement) -> Range<UmbralVersion> {
        if let Some(override_spec) = self.overrides.get(&req.name) {
            specifiers_to_range(override_spec)
        } else {
            req.version
                .as_ref()
                .map(specifiers_to_range)
                .unwrap_or_else(Range::any)
        }
    }

    /// Build the dependency map for a given package and version.
    fn build_dependencies(
        &self,
        package: &UmbralPackage,
        version: &UmbralVersion,
    ) -> Result<Dependencies<UmbralPackage, UmbralVersion>, Box<dyn Error>> {
        match package {
            UmbralPackage::Root => {
                let mut deps: Map<UmbralPackage, Range<UmbralVersion>> = Map::default();

                for req in &self.root_requirements {
                    let range = req
                        .version
                        .as_ref()
                        .map(specifiers_to_range)
                        .unwrap_or_else(Range::any);

                    if req.extras.is_empty() {
                        deps.entry(UmbralPackage::Package(req.name.clone()))
                            .and_modify(|existing| *existing = existing.intersection(&range))
                            .or_insert(range);
                    } else {
                        for extra in &req.extras {
                            let key = UmbralPackage::Extra(req.name.clone(), extra.clone());
                            deps.entry(key)
                                .and_modify(|existing| *existing = existing.intersection(&range))
                                .or_insert(range.clone());
                        }
                    }
                }

                Ok(Dependencies::Known(deps))
            }

            UmbralPackage::Python => {
                // Python has no dependencies
                Ok(Dependencies::Known(Map::default()))
            }

            UmbralPackage::Package(name) => {
                let Some(metadata) = self.source.get_metadata(name, version.inner()) else {
                    return Ok(Dependencies::Unknown);
                };

                let mut deps: Map<UmbralPackage, Range<UmbralVersion>> = Map::default();

                // Model requires-python as a dependency on the Python virtual package.
                // This is CRITICAL: it lets PubGrub backtrack to an older version of the
                // package if the newest version requires a Python newer than the target.
                if let Some(ref requires_python) = metadata.requires_python {
                    let python_range = specifiers_to_range(requires_python);
                    deps.insert(UmbralPackage::Python, python_range);
                }

                // Add regular dependencies (applying overrides where configured)
                for req in &metadata.dependencies {
                    // Skip dependencies with markers that don't match the target environment.
                    // Note: marker evaluation is a todo in pep508; if markers is None,
                    // we include all dependencies.
                    if let (Some(ref marker), Some(ref env)) = (&req.marker, &self.config.markers) {
                        if !marker.evaluate(env) {
                            continue;
                        }
                    }

                    let range = self.range_for_dep(req);

                    if req.extras.is_empty() {
                        deps.entry(UmbralPackage::Package(req.name.clone()))
                            .and_modify(|existing| *existing = existing.intersection(&range))
                            .or_insert(range);
                    } else {
                        for extra in &req.extras {
                            let key = UmbralPackage::Extra(req.name.clone(), extra.clone());
                            deps.entry(key)
                                .and_modify(|existing| *existing = existing.intersection(&range))
                                .or_insert(range.clone());
                        }
                    }
                }

                Ok(Dependencies::Known(deps))
            }

            UmbralPackage::Extra(name, extra) => {
                let Some(metadata) = self.source.get_metadata(name, version.inner()) else {
                    return Ok(Dependencies::Unknown);
                };

                let mut deps: Map<UmbralPackage, Range<UmbralVersion>> = Map::default();

                // The extra virtual package pins the base package to the exact same version.
                deps.insert(
                    UmbralPackage::Package(name.clone()),
                    Range::exact(version.clone()),
                );

                // Add extra-specific dependencies (applying overrides where configured)
                if let Some(extra_deps) = metadata.extras.get(extra) {
                    for req in extra_deps {
                        if let (Some(ref marker), Some(ref env)) =
                            (&req.marker, &self.config.markers)
                        {
                            if !marker.evaluate(env) {
                                continue;
                            }
                        }

                        let range = self.range_for_dep(req);

                        if req.extras.is_empty() {
                            deps.entry(UmbralPackage::Package(req.name.clone()))
                                .and_modify(|existing| *existing = existing.intersection(&range))
                                .or_insert(range);
                        } else {
                            for dep_extra in &req.extras {
                                let key = UmbralPackage::Extra(req.name.clone(), dep_extra.clone());
                                deps.entry(key)
                                    .and_modify(|existing| {
                                        *existing = existing.intersection(&range)
                                    })
                                    .or_insert(range.clone());
                            }
                        }
                    }
                }

                Ok(Dependencies::Known(deps))
            }
        }
    }
}

impl<S: PackageSource> DependencyProvider<UmbralPackage, UmbralVersion> for UmbralProvider<S> {
    fn choose_package_version<T: Borrow<UmbralPackage>, U: Borrow<Range<UmbralVersion>>>(
        &self,
        potential_packages: impl Iterator<Item = (T, U)>,
    ) -> Result<(T, Option<UmbralVersion>), Box<dyn Error>> {
        // Strategy: prioritize packages that will find conflicts fastest.
        // 1. Root and Python first (fixed, immediate decisions)
        // 2. Extras next (virtual, determined by base package)
        // 3. Regular packages last, preferring those with fewest compatible versions
        //    (most constrained = most likely to find conflicts early)

        let mut best: Option<(T, usize, usize, Option<UmbralVersion>)> = None;

        for (pkg, range) in potential_packages {
            let priority = match pkg.borrow() {
                UmbralPackage::Root => 0,
                UmbralPackage::Python => 1,
                UmbralPackage::Extra(_, _) => 2,
                UmbralPackage::Package(_) => 3,
            };

            let versions = self.get_versions(pkg.borrow(), range.borrow());
            let count = versions.len();
            let chosen = versions.into_iter().next(); // first = newest (highest)

            let is_better = match &best {
                None => true,
                Some((_, best_priority, best_count, _)) => {
                    priority < *best_priority || (priority == *best_priority && count < *best_count)
                }
            };

            if is_better {
                best = Some((pkg, priority, count, chosen));
            }
        }

        let (pkg, _, _, version) = best.expect("potential_packages was empty");
        Ok((pkg, version))
    }

    fn get_dependencies(
        &self,
        package: &UmbralPackage,
        version: &UmbralVersion,
    ) -> Result<Dependencies<UmbralPackage, UmbralVersion>, Box<dyn Error>> {
        self.build_dependencies(package, version)
    }

    fn should_cancel(&self) -> Result<(), Box<dyn Error>> {
        let count = self.iteration_count.fetch_add(1, AtomicOrdering::Relaxed);
        if count >= MAX_ITERATIONS {
            Err(format!(
                "resolver exceeded {MAX_ITERATIONS} iterations — \
                 possible pathological dependency graph"
            )
            .into())
        } else {
            Ok(())
        }
    }
}
