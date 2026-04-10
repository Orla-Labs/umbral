//! Resolver error types and human-friendly error formatting.
//!
//! Converts PubGrub's [`DerivationTree`] into clear, actionable error messages
//! with contextual hints (e.g., upgrade Python, relax a constraint).

use std::fmt;

use indexmap::IndexSet;
use pubgrub::package::Package;
use pubgrub::range::Range;
use pubgrub::report::{DerivationTree, Derived, External, Reporter};
use pubgrub::term::Term;
use pubgrub::type_aliases::Map;
use pubgrub::version::Version;
use thiserror::Error;

// ── Resolver error ─────────────────────────────────────────────────

/// Top-level error type for the resolver.
#[derive(Debug, Error)]
pub enum ResolverError {
    #[error("dependency resolution failed:\n{0}")]
    NoSolution(ResolutionReport),

    #[error("failed to fetch dependencies for {package} {version}: {reason}")]
    DependencyFetch {
        package: String,
        version: String,
        reason: String,
    },

    #[error("{package} {version} has a dependency on the empty set: {dependent}")]
    EmptyDependency {
        package: String,
        version: String,
        dependent: String,
    },

    #[error("{package} {version} depends on itself")]
    SelfDependency { package: String, version: String },

    #[error("failed to choose package version: {0}")]
    PackageSelection(String),

    #[error("cancelled: {0}")]
    Cancelled(String),

    #[error("{0}")]
    Other(String),
}

impl ResolverError {
    /// Convert a `PubGrubError` into an `ResolverError`.
    pub fn from_pubgrub<P: Package, V: Version>(err: pubgrub::error::PubGrubError<P, V>) -> Self {
        match err {
            pubgrub::error::PubGrubError::NoSolution(mut tree) => {
                tree.collapse_no_versions();
                let report = UmbralReporter::report(&tree);
                ResolverError::NoSolution(report)
            }
            pubgrub::error::PubGrubError::ErrorRetrievingDependencies {
                package,
                version,
                source,
            } => ResolverError::DependencyFetch {
                package: package.to_string(),
                version: version.to_string(),
                reason: source.to_string(),
            },
            pubgrub::error::PubGrubError::DependencyOnTheEmptySet {
                package,
                version,
                dependent,
            } => ResolverError::EmptyDependency {
                package: package.to_string(),
                version: version.to_string(),
                dependent: dependent.to_string(),
            },
            pubgrub::error::PubGrubError::SelfDependency { package, version } => {
                ResolverError::SelfDependency {
                    package: package.to_string(),
                    version: version.to_string(),
                }
            }
            pubgrub::error::PubGrubError::ErrorChoosingPackageVersion(e) => {
                ResolverError::PackageSelection(e.to_string())
            }
            pubgrub::error::PubGrubError::ErrorInShouldCancel(e) => {
                ResolverError::Cancelled(e.to_string())
            }
            pubgrub::error::PubGrubError::Failure(msg) => ResolverError::Other(msg),
        }
    }
}

// ── Resolution report ──────────────────────────────────────────────

/// A structured report explaining why dependency resolution failed,
/// including the derivation explanation and actionable hints.
#[derive(Debug, Clone)]
pub struct ResolutionReport {
    /// Human-readable explanation chain (e.g. "Because X and Y, Z.")
    pub explanation: String,
    /// Actionable hints the user might follow to fix the conflict.
    pub hints: Vec<Hint>,
}

impl fmt::Display for ResolutionReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.explanation)?;
        if !self.hints.is_empty() {
            writeln!(f)?;
            for hint in &self.hints {
                write!(f, "\n  hint: {hint}")?;
            }
        }
        Ok(())
    }
}

/// An actionable suggestion for resolving a dependency conflict.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Hint {
    /// The conflict involves a Python version constraint that may be relaxed
    /// by upgrading the target Python.
    UpgradePython {
        constraint: String,
        /// The package (and its version range) that requires the incompatible
        /// Python version. E.g. `Some(("numpy", ">=2.0"))`.
        blocking_package: Option<(String, String)>,
    },
    /// A package has no versions matching the requested range — the user
    /// might relax the constraint.
    RelaxConstraint { package: String, range: String },
    /// Dependencies for a package were unavailable — check index configuration.
    CheckIndex { package: String },
    /// A pre-release version might satisfy the constraint — pass `--pre`.
    TryPreRelease { package: String },
    /// The package only provides source distributions (sdists), no wheels.
    /// The optional second field is a platform tag string describing what
    /// platform tags were tried (e.g. `cp312-cp312-macosx_14_0_arm64`).
    SdistOnly {
        package: String,
        platform_tag: Option<String>,
    },
}

impl fmt::Display for Hint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Hint::UpgradePython {
                constraint,
                blocking_package,
            } => {
                if let Some((pkg, pkg_range)) = blocking_package {
                    write!(
                        f,
                        "the target Python version may be too old — \
                         {pkg} {pkg_range} requires Python {constraint}. \
                         Consider upgrading Python or pinning {pkg} to an older version"
                    )
                } else {
                    write!(
                        f,
                        "the project requires Python {constraint} — consider upgrading \
                         your Python version or relaxing the requires-python constraint"
                    )
                }
            }
            Hint::RelaxConstraint { package, range } => {
                write!(
                    f,
                    "no versions of {package} match {range} — consider relaxing \
                     the version constraint"
                )
            }
            Hint::CheckIndex { package } => {
                write!(
                    f,
                    "dependencies of {package} are unavailable — check your \
                     --index-url and network connectivity"
                )
            }
            Hint::TryPreRelease { package } => {
                write!(
                    f,
                    "a pre-release version of {package} might satisfy the constraint \
                     — try passing --pre"
                )
            }
            Hint::SdistOnly {
                package,
                platform_tag,
            } => {
                write!(
                    f,
                    "package \"{package}\" only has source distributions (sdist), \
                     no pre-built wheels"
                )?;
                if let Some(tag) = platform_tag {
                    write!(f, " available for {tag}")?;
                }
                write!(f, ". Umbral does not yet support building from source.")
            }
        }
    }
}

// ── Custom reporter ────────────────────────────────────────────────

/// Umbral's custom PubGrub reporter producing [`ResolutionReport`]s.
///
/// Extends PubGrub's `DefaultStringReporter` with:
/// - Improved natural-language phrasing
/// - Contextual hints extracted from the derivation tree
pub struct UmbralReporter {
    ref_count: usize,
    shared_with_ref: Map<usize, usize>,
    lines: Vec<String>,
    hints: IndexSet<Hint>,
}

impl UmbralReporter {
    fn new() -> Self {
        Self {
            ref_count: 0,
            shared_with_ref: Map::default(),
            lines: Vec::new(),
            hints: IndexSet::new(),
        }
    }

    // ── Tree walking ───────────────────────────────────────────

    fn build_recursive<P: Package, V: Version>(&mut self, derived: &Derived<P, V>) {
        self.build_recursive_helper(derived);
        if let Some(id) = derived.shared_id {
            if !self.shared_with_ref.contains_key(&id) {
                self.add_line_ref();
                self.shared_with_ref.insert(id, self.ref_count);
            }
        }
    }

    fn build_recursive_helper<P: Package, V: Version>(&mut self, current: &Derived<P, V>) {
        match (current.cause1.as_ref(), current.cause2.as_ref()) {
            (DerivationTree::External(ext1), DerivationTree::External(ext2)) => {
                self.collect_hints_external(ext1);
                self.collect_hints_external(ext2);
                self.lines
                    .push(Self::explain_both_external(ext1, ext2, &current.terms));
            }
            (DerivationTree::Derived(derived), DerivationTree::External(external))
            | (DerivationTree::External(external), DerivationTree::Derived(derived)) => {
                self.collect_hints_external(external);
                self.report_one_each(derived, external, &current.terms);
            }
            (DerivationTree::Derived(derived1), DerivationTree::Derived(derived2)) => {
                match (
                    self.line_ref_of(derived1.shared_id),
                    self.line_ref_of(derived2.shared_id),
                ) {
                    (Some(ref1), Some(ref2)) => {
                        self.lines.push(Self::explain_both_ref(
                            ref1,
                            derived1,
                            ref2,
                            derived2,
                            &current.terms,
                        ));
                    }
                    (Some(ref1), None) => {
                        self.build_recursive(derived2);
                        self.lines
                            .push(Self::and_explain_ref(ref1, derived1, &current.terms));
                    }
                    (None, Some(ref2)) => {
                        self.build_recursive(derived1);
                        self.lines
                            .push(Self::and_explain_ref(ref2, derived2, &current.terms));
                    }
                    (None, None) => {
                        self.build_recursive(derived1);
                        if derived1.shared_id.is_some() {
                            self.lines.push(String::new());
                            self.build_recursive(current);
                        } else {
                            self.add_line_ref();
                            let ref1 = self.ref_count;
                            self.lines.push(String::new());
                            self.build_recursive(derived2);
                            self.lines
                                .push(Self::and_explain_ref(ref1, derived1, &current.terms));
                        }
                    }
                }
            }
        }
    }

    fn report_one_each<P: Package, V: Version>(
        &mut self,
        derived: &Derived<P, V>,
        external: &External<P, V>,
        current_terms: &Map<P, Term<V>>,
    ) {
        match self.line_ref_of(derived.shared_id) {
            Some(ref_id) => {
                self.lines.push(Self::explain_ref_and_external(
                    ref_id,
                    derived,
                    external,
                    current_terms,
                ));
            }
            None => self.report_recurse_one_each(derived, external, current_terms),
        }
    }

    fn report_recurse_one_each<P: Package, V: Version>(
        &mut self,
        derived: &Derived<P, V>,
        external: &External<P, V>,
        current_terms: &Map<P, Term<V>>,
    ) {
        match (derived.cause1.as_ref(), derived.cause2.as_ref()) {
            (DerivationTree::Derived(prior_derived), DerivationTree::External(prior_external))
            | (DerivationTree::External(prior_external), DerivationTree::Derived(prior_derived)) => {
                self.build_recursive(prior_derived);
                self.lines.push(Self::and_explain_prior_and_external(
                    prior_external,
                    external,
                    current_terms,
                ));
            }
            _ => {
                self.build_recursive(derived);
                self.lines
                    .push(Self::and_explain_external(external, current_terms));
            }
        }
    }

    // ── Formatting helpers ─────────────────────────────────────

    fn explain_both_external<P: Package, V: Version>(
        external1: &External<P, V>,
        external2: &External<P, V>,
        current_terms: &Map<P, Term<V>>,
    ) -> String {
        format!(
            "Because {} and {}, {}.",
            Self::format_external(external1),
            Self::format_external(external2),
            Self::format_terms(current_terms)
        )
    }

    fn explain_both_ref<P: Package, V: Version>(
        ref1: usize,
        derived1: &Derived<P, V>,
        ref2: usize,
        derived2: &Derived<P, V>,
        current_terms: &Map<P, Term<V>>,
    ) -> String {
        format!(
            "Because {} ({}) and {} ({}), {}.",
            Self::format_terms(&derived1.terms),
            ref1,
            Self::format_terms(&derived2.terms),
            ref2,
            Self::format_terms(current_terms)
        )
    }

    fn explain_ref_and_external<P: Package, V: Version>(
        ref_id: usize,
        derived: &Derived<P, V>,
        external: &External<P, V>,
        current_terms: &Map<P, Term<V>>,
    ) -> String {
        format!(
            "Because {} ({}) and {}, {}.",
            Self::format_terms(&derived.terms),
            ref_id,
            Self::format_external(external),
            Self::format_terms(current_terms)
        )
    }

    fn and_explain_external<P: Package, V: Version>(
        external: &External<P, V>,
        current_terms: &Map<P, Term<V>>,
    ) -> String {
        format!(
            "And because {}, {}.",
            Self::format_external(external),
            Self::format_terms(current_terms)
        )
    }

    fn and_explain_ref<P: Package, V: Version>(
        ref_id: usize,
        derived: &Derived<P, V>,
        current_terms: &Map<P, Term<V>>,
    ) -> String {
        format!(
            "And because {} ({}), {}.",
            Self::format_terms(&derived.terms),
            ref_id,
            Self::format_terms(current_terms)
        )
    }

    fn and_explain_prior_and_external<P: Package, V: Version>(
        prior_external: &External<P, V>,
        external: &External<P, V>,
        current_terms: &Map<P, Term<V>>,
    ) -> String {
        format!(
            "And because {} and {}, {}.",
            Self::format_external(prior_external),
            Self::format_external(external),
            Self::format_terms(current_terms)
        )
    }

    /// Format an external incompatibility with improved phrasing.
    fn format_external<P: Package, V: Version>(external: &External<P, V>) -> String {
        match external {
            External::NotRoot(package, version) => {
                format!("your project depends on {package} {version}")
            }
            External::NoVersions(package, range) => {
                if range == &Range::any() {
                    format!("no versions of {package} are available")
                } else {
                    format!("no versions of {package} match {range}")
                }
            }
            External::UnavailableDependencies(package, range) => {
                if range == &Range::any() {
                    format!("dependencies of {package} are unavailable")
                } else {
                    format!("dependencies of {package} {range} are unavailable")
                }
            }
            External::FromDependencyOf(package, pkg_range, dep, dep_range) => {
                if pkg_range == &Range::any() && dep_range == &Range::any() {
                    format!("{package} depends on {dep}")
                } else if pkg_range == &Range::any() {
                    format!("{package} requires {dep} {dep_range}")
                } else if dep_range == &Range::any() {
                    format!("{package} {pkg_range} depends on {dep}")
                } else {
                    format!("{package} {pkg_range} requires {dep} {dep_range}")
                }
            }
        }
    }

    /// Format terms into a human-readable conclusion.
    fn format_terms<P: Package, V: Version>(terms: &Map<P, Term<V>>) -> String {
        let terms_vec: Vec<_> = terms.iter().collect();
        match terms_vec.as_slice() {
            [] => "version solving failed".into(),
            [(package, Term::Positive(range))] => {
                if range == &Range::any() {
                    format!("{package} cannot be used")
                } else {
                    format!("{package} {range} is forbidden")
                }
            }
            [(package, Term::Negative(range))] => {
                if range == &Range::any() {
                    format!("{package} is required")
                } else {
                    format!("{package} {range} is required")
                }
            }
            [(p1, Term::Positive(r1)), (p2, Term::Negative(r2))] => {
                Self::format_external(&External::FromDependencyOf(p1, r1.clone(), p2, r2.clone()))
            }
            [(p1, Term::Negative(r1)), (p2, Term::Positive(r2))] => {
                Self::format_external(&External::FromDependencyOf(p2, r2.clone(), p1, r1.clone()))
            }
            slice => {
                let parts: Vec<_> = slice.iter().map(|(p, t)| format!("{p} {t}")).collect();
                format!("{} are incompatible", parts.join(", "))
            }
        }
    }

    // ── Hint extraction ────────────────────────────────────────

    /// Scan an external incompatibility and generate hints.
    fn collect_hints_external<P: Package, V: Version>(&mut self, external: &External<P, V>) {
        match external {
            External::NoVersions(package, range) => {
                let pkg = package.to_string();

                // Python-related package → suggest upgrading Python
                if is_python_constraint(&pkg) {
                    self.hints.insert(Hint::UpgradePython {
                        constraint: range.to_string(),
                        blocking_package: None,
                    });
                    return;
                }

                // General "no versions" → suggest relaxing constraint
                self.hints.insert(Hint::RelaxConstraint {
                    package: pkg.clone(),
                    range: range.to_string(),
                });

                // Also suggest trying pre-releases
                self.hints.insert(Hint::TryPreRelease { package: pkg });
            }
            External::UnavailableDependencies(package, _) => {
                self.hints.insert(Hint::CheckIndex {
                    package: package.to_string(),
                });
            }
            External::FromDependencyOf(package, pkg_range, dep, dep_range) => {
                let dep_str = dep.to_string();
                if is_python_constraint(&dep_str) && dep_range != &Range::any() {
                    let pkg_str = package.to_string();
                    let blocking = if !is_root_package(&pkg_str) {
                        Some((pkg_str, pkg_range.to_string()))
                    } else {
                        None
                    };
                    self.hints.insert(Hint::UpgradePython {
                        constraint: dep_range.to_string(),
                        blocking_package: blocking,
                    });
                }
            }
            _ => {}
        }
    }

    // ── Ref tracking ───────────────────────────────────────────

    fn add_line_ref(&mut self) {
        self.ref_count += 1;
        if let Some(line) = self.lines.last_mut() {
            *line = format!("{line} ({})", self.ref_count);
        }
    }

    fn line_ref_of(&self, shared_id: Option<usize>) -> Option<usize> {
        shared_id.and_then(|id| self.shared_with_ref.get(&id).copied())
    }
}

/// Check whether a package name represents a Python interpreter constraint.
fn is_python_constraint(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower == "python" || lower == "python_version" || lower == "cpython"
}

/// Check whether a package name represents the root project.
fn is_root_package(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower == "root" || lower == "<root>"
}

impl<P: Package, V: Version> Reporter<P, V> for UmbralReporter {
    type Output = ResolutionReport;

    fn report(derivation_tree: &DerivationTree<P, V>) -> Self::Output {
        match derivation_tree {
            DerivationTree::External(external) => {
                let mut reporter = Self::new();
                reporter.collect_hints_external(external);
                ResolutionReport {
                    explanation: Self::format_external(external),
                    hints: reporter.hints.into_iter().collect(),
                }
            }
            DerivationTree::Derived(derived) => {
                let mut reporter = Self::new();
                reporter.build_recursive(derived);
                ResolutionReport {
                    explanation: reporter.lines.join("\n"),
                    hints: reporter.hints.into_iter().collect(),
                }
            }
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use pubgrub::range::Range;
    use pubgrub::report::{DerivationTree, Derived, External};
    use pubgrub::version::NumberVersion;

    type DT = DerivationTree<String, NumberVersion>;

    fn root_dep(pkg: &str, range: Range<NumberVersion>) -> DT {
        DerivationTree::External(External::FromDependencyOf(
            "root".into(),
            Range::exact(1),
            pkg.into(),
            range,
        ))
    }

    fn pkg_dep(
        from: &str,
        from_range: Range<NumberVersion>,
        to: &str,
        to_range: Range<NumberVersion>,
    ) -> DT {
        DerivationTree::External(External::FromDependencyOf(
            from.into(),
            from_range,
            to.into(),
            to_range,
        ))
    }

    fn no_versions(pkg: &str, range: Range<NumberVersion>) -> DT {
        DerivationTree::External(External::NoVersions(pkg.into(), range))
    }

    fn unavailable(pkg: &str) -> DT {
        DerivationTree::External(External::UnavailableDependencies(pkg.into(), Range::any()))
    }

    fn derived(cause1: DT, cause2: DT, terms: Map<String, Term<NumberVersion>>) -> DT {
        DerivationTree::Derived(Derived {
            terms,
            shared_id: None,
            cause1: Box::new(cause1),
            cause2: Box::new(cause2),
        })
    }

    // ── Snapshot tests ─────────────────────────────────────────

    #[test]
    fn simple_conflict_two_externals() {
        // root requires foo >=2 and bar requires foo <2 → conflict
        let tree = derived(
            root_dep("foo", Range::higher_than(2)),
            pkg_dep("bar", Range::any(), "foo", Range::strictly_lower_than(2)),
            Map::default(),
        );

        let report = UmbralReporter::report(&tree);
        insta::assert_snapshot!(report.explanation, @"Because root 1 requires foo 2 <= v and bar requires foo v < 2, version solving failed.");
    }

    #[test]
    fn no_versions_available() {
        let tree = no_versions("nonexistent", Range::any());
        let report = UmbralReporter::report(&tree);
        insta::assert_snapshot!(report.explanation, @"no versions of nonexistent are available");
        assert!(report
            .hints
            .iter()
            .any(|h| matches!(h, Hint::RelaxConstraint { .. })));
        assert!(report
            .hints
            .iter()
            .any(|h| matches!(h, Hint::TryPreRelease { .. })));
    }

    #[test]
    fn no_versions_in_range() {
        let tree = no_versions("requests", Range::higher_than(99));
        let report = UmbralReporter::report(&tree);
        insta::assert_snapshot!(report.explanation, @"no versions of requests match 99 <= v");
        assert!(report.hints.iter().any(|h| matches!(
            h,
            Hint::RelaxConstraint { package, .. } if package == "requests"
        )));
    }

    #[test]
    fn unavailable_dependencies_hint() {
        let tree = unavailable("private-pkg");
        let report = UmbralReporter::report(&tree);
        insta::assert_snapshot!(report.explanation, @"dependencies of private-pkg are unavailable");
        assert!(report.hints.iter().any(|h| matches!(
            h,
            Hint::CheckIndex { package } if package == "private-pkg"
        )));
    }

    #[test]
    fn python_version_hint() {
        // Simulate: package requires python >=3.12 but no version available
        let tree = no_versions("python", Range::higher_than(12));
        let report = UmbralReporter::report(&tree);
        assert!(
            report
                .hints
                .iter()
                .any(|h| matches!(h, Hint::UpgradePython { .. })),
            "expected UpgradePython hint, got: {:?}",
            report.hints
        );
    }

    #[test]
    fn derived_conflict_with_chain() {
        // root -> foo >=2
        // foo >=2 -> bar >=3
        // root -> bar <3
        // Result: derived from (derived from root->foo, foo->bar) + root->bar<3
        let inner_terms = Map::default();
        let inner = derived(
            root_dep("foo", Range::higher_than(2)),
            pkg_dep("foo", Range::higher_than(2), "bar", Range::higher_than(3)),
            inner_terms,
        );

        let outer_terms = Map::default();
        let tree = derived(
            inner,
            root_dep("bar", Range::strictly_lower_than(3)),
            outer_terms,
        );

        let report = UmbralReporter::report(&tree);
        // Should have a multi-line explanation with "Because" and "And because"
        assert!(
            report.explanation.contains("Because"),
            "expected 'Because' in: {}",
            report.explanation
        );
        assert!(
            report.explanation.contains("And because"),
            "expected 'And because' in: {}",
            report.explanation
        );
    }

    #[test]
    fn hint_display_formatting() {
        let hints = vec![
            Hint::UpgradePython {
                constraint: ">= 3.12".into(),
                blocking_package: None,
            },
            Hint::RelaxConstraint {
                package: "numpy".into(),
                range: ">= 99.0".into(),
            },
            Hint::CheckIndex {
                package: "private-pkg".into(),
            },
            Hint::TryPreRelease {
                package: "torch".into(),
            },
        ];

        let formatted: Vec<String> = hints.iter().map(|h| h.to_string()).collect();
        insta::assert_snapshot!(formatted.join("\n"), @r"
        the project requires Python >= 3.12 — consider upgrading your Python version or relaxing the requires-python constraint
        no versions of numpy match >= 99.0 — consider relaxing the version constraint
        dependencies of private-pkg are unavailable — check your --index-url and network connectivity
        a pre-release version of torch might satisfy the constraint — try passing --pre
        ");
    }

    #[test]
    fn report_display_includes_hints() {
        let report = ResolutionReport {
            explanation: "Because foo requires bar >= 2 and your project requires bar < 1, version solving failed.".into(),
            hints: vec![
                Hint::RelaxConstraint {
                    package: "bar".into(),
                    range: "< 1".into(),
                },
            ],
        };
        let display = report.to_string();
        assert!(display.contains("Because foo"));
        assert!(display.contains("hint:"));
        assert!(display.contains("bar"));
    }

    #[test]
    fn report_display_no_hints() {
        let report = ResolutionReport {
            explanation: "Something went wrong.".into(),
            hints: vec![],
        };
        assert_eq!(report.to_string(), "Something went wrong.");
    }

    // ── Improvement 1: SdistOnly hint includes platform tag ───────

    #[test]
    fn sdist_only_hint_with_platform_tag() {
        let hint = Hint::SdistOnly {
            package: "cryptography".into(),
            platform_tag: Some("cp312-cp312-macosx_14_0_arm64".into()),
        };
        let msg = hint.to_string();
        assert!(
            msg.contains("cryptography"),
            "should mention the package name: {msg}"
        );
        assert!(
            msg.contains("cp312-cp312-macosx_14_0_arm64"),
            "should include the platform tag: {msg}"
        );
        assert!(
            msg.contains("source distributions"),
            "should mention sdist: {msg}"
        );
        insta::assert_snapshot!(msg, @r#"package "cryptography" only has source distributions (sdist), no pre-built wheels available for cp312-cp312-macosx_14_0_arm64. Umbral does not yet support building from source."#);
    }

    #[test]
    fn sdist_only_hint_without_platform_tag() {
        let hint = Hint::SdistOnly {
            package: "legacy-pkg".into(),
            platform_tag: None,
        };
        let msg = hint.to_string();
        assert!(
            msg.contains("legacy-pkg"),
            "should mention the package name: {msg}"
        );
        assert!(
            !msg.contains("available for"),
            "should not include platform clause when tag is None: {msg}"
        );
        insta::assert_snapshot!(msg, @r#"package "legacy-pkg" only has source distributions (sdist), no pre-built wheels. Umbral does not yet support building from source."#);
    }

    // ── Improvement 2: UpgradePython shows blocking package ───────

    #[test]
    fn upgrade_python_hint_with_blocking_package() {
        // Simulate: numpy >=2.0 requires Python >=3.12
        let tree = derived(
            pkg_dep(
                "numpy",
                Range::higher_than(2),
                "python",
                Range::higher_than(12),
            ),
            no_versions("python", Range::higher_than(12)),
            Map::default(),
        );

        let report = UmbralReporter::report(&tree);
        let upgrade_hint = report
            .hints
            .iter()
            .find(|h| matches!(h, Hint::UpgradePython { .. }));
        assert!(
            upgrade_hint.is_some(),
            "expected UpgradePython hint, got: {:?}",
            report.hints
        );

        let msg = upgrade_hint.unwrap().to_string();
        assert!(
            msg.contains("numpy"),
            "should mention the blocking package 'numpy': {msg}"
        );
        assert!(
            msg.contains("pinning numpy"),
            "should suggest pinning the blocking package: {msg}"
        );
    }

    #[test]
    fn upgrade_python_hint_from_root_has_no_blocking_package() {
        // When root itself depends on Python, there's no "blocking package"
        // — it's the project's own requires-python constraint.
        let tree = derived(
            root_dep("python", Range::higher_than(12)),
            no_versions("python", Range::higher_than(12)),
            Map::default(),
        );

        let report = UmbralReporter::report(&tree);
        let upgrade_hint = report
            .hints
            .iter()
            .find(|h| matches!(h, Hint::UpgradePython { .. }));
        assert!(
            upgrade_hint.is_some(),
            "expected UpgradePython hint, got: {:?}",
            report.hints
        );

        let msg = upgrade_hint.unwrap().to_string();
        // Root dependency → generic message, no specific blocking package
        assert!(
            msg.contains("requires Python"),
            "should mention Python constraint: {msg}"
        );
        assert!(
            msg.contains("consider upgrading"),
            "should suggest upgrading: {msg}"
        );
    }
}
