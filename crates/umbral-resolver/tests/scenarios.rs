//! Integration test scenarios for the Umbral resolver.
//!
//! These tests exercise the high-level `umbral_resolver::resolve()` function
//! with `MockRegistry` to validate end-to-end resolution behavior.
//! They complement the unit tests in lib.rs with additional edge cases.

use std::collections::HashMap;

use umbral_pep440::PackageName;
use umbral_pep508::Requirement;

use umbral_resolver::error::Hint;
use umbral_resolver::mock::MockRegistry;
use umbral_resolver::{resolve, PreReleasePolicy, ResolverConfig, ResolverError};

// ── Helpers ────────────────────────────────────────────────────────

fn cfg(python: &str) -> ResolverConfig {
    ResolverConfig {
        python_version: python.parse().unwrap(),
        markers: None,
        pre_release_policy: PreReleasePolicy::Disallow,
    }
}

fn cfg_pre(python: &str) -> ResolverConfig {
    ResolverConfig {
        python_version: python.parse().unwrap(),
        markers: None,
        pre_release_policy: PreReleasePolicy::Allow,
    }
}

fn r(name: &str, spec: &str) -> Requirement {
    Requirement {
        name: PackageName::new(name),
        extras: vec![],
        version: if spec.is_empty() {
            None
        } else {
            Some(spec.parse().unwrap())
        },
        url: None,
        marker: None,
    }
}

fn r_extra(name: &str, extras: Vec<&str>, spec: &str) -> Requirement {
    Requirement {
        name: PackageName::new(name),
        extras: extras.into_iter().map(|s| s.to_string()).collect(),
        version: if spec.is_empty() {
            None
        } else {
            Some(spec.parse().unwrap())
        },
        url: None,
        marker: None,
    }
}

fn assert_version(graph: &umbral_resolver::ResolutionGraph, name: &str, version: &str) {
    let pkg = &graph.packages[&PackageName::new(name)];
    assert_eq!(
        pkg.version.to_string(),
        version,
        "expected {name}=={version}, got {name}=={}",
        pkg.version
    );
}

// ── Scenario: Empty dependency list resolves trivially ──────────────

#[test]
fn empty_dependencies() {
    let reg = MockRegistry::new();
    let result = resolve(reg, cfg("3.11"), vec![]).unwrap();
    assert!(result.packages.is_empty());
}

// ── Scenario: All yanked → resolution fails ────────────────────────

#[test]
fn all_versions_yanked() {
    let mut reg = MockRegistry::new();
    reg.add_yanked_version("pkg", "1.0.0", vec![])
        .add_yanked_version("pkg", "2.0.0", vec![]);

    let result = resolve(reg, cfg("3.11"), vec![r("pkg", ">=1.0")]);
    assert!(result.is_err());
}

// ── Scenario: Deep chain (5 levels) resolves ────────────────────────

#[test]
fn deep_transitive_chain() {
    let mut reg = MockRegistry::new();
    reg.add_version("a", "1.0.0", vec![("b", ">=1.0")])
        .add_version("b", "1.0.0", vec![("c", ">=1.0")])
        .add_version("c", "1.0.0", vec![("d", ">=1.0")])
        .add_version("d", "1.0.0", vec![("e", ">=1.0")])
        .add_version("e", "1.0.0", vec![]);

    let result = resolve(reg, cfg("3.11"), vec![r("a", ">=1.0")]).unwrap();

    assert_eq!(result.packages.len(), 5);
    for name in &["a", "b", "c", "d", "e"] {
        assert!(result.packages.contains_key(&PackageName::new(*name)));
    }
}

// ── Scenario: Pre-release is only option → fails without --pre ─────

#[test]
fn only_prerelease_available_blocked() {
    // pkg has only 2.0.0a1, and root requires >=1.0
    // Without --pre, the pre-release is filtered → no versions match
    let mut reg = MockRegistry::new();
    reg.add_version("pkg", "2.0.0a1", vec![]);

    let result = resolve(reg, cfg("3.11"), vec![r("pkg", ">=1.0")]);
    assert!(result.is_err());
}

#[test]
fn only_prerelease_available_allowed() {
    // Same setup but with --pre → 2.0.0a1 is selectable
    // (2.0.0a1 >= 1.0 in PEP 440, since 2.0.0a1 > 1.0.0)
    let mut reg = MockRegistry::new();
    reg.add_version("pkg", "2.0.0a1", vec![]);

    let result = resolve(reg, cfg_pre("3.11"), vec![r("pkg", ">=1.0")]).unwrap();
    assert_version(&result, "pkg", "2.0.0a1");
}

// ── Scenario: Python backtrack picks oldest compatible ──────────────

#[test]
fn python_backtrack_three_versions_low() {
    let mut reg = MockRegistry::new();
    reg.add_version_with_python("pkg", "3.0.0", vec![], ">=3.13")
        .add_version_with_python("pkg", "2.0.0", vec![], ">=3.12")
        .add_version_with_python("pkg", "1.0.0", vec![], ">=3.8");

    // Python 3.10 → only v1.0.0 works
    let result = resolve(reg, cfg("3.10"), vec![r("pkg", ">=1.0")]).unwrap();
    assert_version(&result, "pkg", "1.0.0");
}

#[test]
fn python_backtrack_three_versions_mid() {
    let mut reg = MockRegistry::new();
    reg.add_version_with_python("pkg", "3.0.0", vec![], ">=3.13")
        .add_version_with_python("pkg", "2.0.0", vec![], ">=3.12")
        .add_version_with_python("pkg", "1.0.0", vec![], ">=3.8");

    // Python 3.12 → v2.0.0 is best
    let result = resolve(reg, cfg("3.12"), vec![r("pkg", ">=1.0")]).unwrap();
    assert_version(&result, "pkg", "2.0.0");
}

// ── Scenario: Extras pull in extra deps ─────────────────────────────

#[test]
fn extras_with_multiple_extra_deps() {
    let mut reg = MockRegistry::new();
    reg.add_version_with_extras(
        "requests",
        "2.31.0",
        vec![("urllib3", ">=1.21"), ("charset-normalizer", ">=2.0")],
        HashMap::from([
            ("socks".to_string(), vec![("pysocks", ">=1.5")]),
            (
                "security".to_string(),
                vec![("pyopenssl", ">=20.0"), ("cryptography", ">=38.0")],
            ),
        ]),
    )
    .add_version("urllib3", "2.1.0", vec![])
    .add_version("charset-normalizer", "3.3.0", vec![])
    .add_version("pysocks", "1.7.1", vec![])
    .add_version("pyopenssl", "23.3.0", vec![])
    .add_version("cryptography", "41.0.0", vec![]);

    // Install requests[security]
    let result = resolve(
        reg,
        cfg("3.11"),
        vec![r_extra("requests", vec!["security"], ">=2.0")],
    )
    .unwrap();

    assert_version(&result, "requests", "2.31.0");
    assert_version(&result, "urllib3", "2.1.0");
    assert_version(&result, "charset-normalizer", "3.3.0");
    assert_version(&result, "pyopenssl", "23.3.0");
    assert_version(&result, "cryptography", "41.0.0");
    // pysocks should NOT be included (only socks extra pulls it)
    assert!(!result.packages.contains_key(&PackageName::new("pysocks")));
}

// ── Scenario: Diamond with multiple compatible versions ─────────────

#[test]
fn diamond_picks_newest_compatible() {
    // root -> A, root -> B
    // A requires C >= 1.0, < 3.0
    // B requires C >= 2.0
    // C has 1.0, 2.0, 3.0
    // Should pick C 2.0.0 (satisfies both)
    let mut reg = MockRegistry::new();
    reg.add_version("a", "1.0.0", vec![("c", ">=1.0,<3.0")])
        .add_version("b", "1.0.0", vec![("c", ">=2.0")])
        .add_version("c", "1.0.0", vec![])
        .add_version("c", "2.0.0", vec![])
        .add_version("c", "3.0.0", vec![]);

    let result = resolve(reg, cfg("3.11"), vec![r("a", ">=1.0"), r("b", ">=1.0")]).unwrap();

    assert_version(&result, "c", "2.0.0");
}

// ── Scenario: Error report has explanation and hints ─────────────────

#[test]
fn conflict_error_has_explanation() {
    let mut reg = MockRegistry::new();
    reg.add_version("a", "1.0.0", vec![("c", ">=2.0")])
        .add_version("b", "1.0.0", vec![("c", "<2.0")])
        .add_version("c", "1.0.0", vec![])
        .add_version("c", "2.0.0", vec![]);

    let err = resolve(reg, cfg("3.11"), vec![r("a", ">=1.0"), r("b", ">=1.0")]).unwrap_err();

    let display = err.to_string();
    assert!(display.contains("Because"), "error: {display}");
    assert!(display.contains("resolution failed"), "error: {display}");
}

#[test]
fn python_conflict_produces_hint() {
    let mut reg = MockRegistry::new();
    reg.add_version_with_python("pkg", "1.0.0", vec![], ">=3.15");

    let err = resolve(reg, cfg("3.10"), vec![r("pkg", ">=1.0")]).unwrap_err();

    if let ResolverError::NoSolution(report) = &err {
        assert!(
            report
                .hints
                .iter()
                .any(|h| matches!(h, Hint::UpgradePython { .. })),
            "should have UpgradePython hint, got: {:?}",
            report.hints
        );
    }
}

// ── Scenario: Shared dependency deduplication ───────────────────────

#[test]
fn shared_dependency_resolved_once() {
    // A -> C, B -> C, D -> C — C should appear once in the solution
    let mut reg = MockRegistry::new();
    reg.add_version("a", "1.0.0", vec![("c", ">=1.0")])
        .add_version("b", "1.0.0", vec![("c", ">=1.0")])
        .add_version("d", "1.0.0", vec![("c", ">=1.0")])
        .add_version("c", "1.0.0", vec![]);

    let result = resolve(
        reg,
        cfg("3.11"),
        vec![r("a", ">=1.0"), r("b", ">=1.0"), r("d", ">=1.0")],
    )
    .unwrap();

    assert_eq!(result.packages.len(), 4); // a, b, c, d
    assert_version(&result, "c", "1.0.0");
}

// ── Scenario: Backtracking chooses best compatible set ───────────────

#[test]
fn backtracking_picks_newest_compatible_set() {
    // A v3 -> B >= 3 (no B v3)
    // A v2 -> B >= 2
    // A v1 -> B >= 1
    // B v2 available
    // Should pick A v2, B v2
    let mut reg = MockRegistry::new();
    reg.add_version("a", "3.0.0", vec![("b", ">=3.0")])
        .add_version("a", "2.0.0", vec![("b", ">=2.0")])
        .add_version("a", "1.0.0", vec![("b", ">=1.0")])
        .add_version("b", "2.0.0", vec![]);

    let result = resolve(reg, cfg("3.11"), vec![r("a", ">=1.0")]).unwrap();

    assert_version(&result, "a", "2.0.0");
    assert_version(&result, "b", "2.0.0");
}

// ── Scenario: Diamond dependency with version conflict ─────────────

#[test]
fn diamond_conflict_produces_no_solution() {
    // root -> A, root -> B
    // A requires C >= 2.0
    // B requires C < 2.0
    // C has 1.0.0 and 2.0.0
    // No version of C satisfies both constraints.
    let mut reg = MockRegistry::new();
    reg.add_version("a", "1.0.0", vec![("c", ">=2.0")])
        .add_version("b", "1.0.0", vec![("c", "<2.0")])
        .add_version("c", "1.0.0", vec![])
        .add_version("c", "2.0.0", vec![]);

    let err = resolve(reg, cfg("3.11"), vec![r("a", ">=1.0"), r("b", ">=1.0")]).unwrap_err();

    let display = err.to_string();
    assert!(
        display.contains("resolution failed") || display.contains("Because"),
        "should produce a NoSolution error, got: {display}"
    );
}

// ── Scenario: Extras with transitive dependencies ──────────────────

#[test]
fn extras_pull_transitive_deps() {
    // A[extra1] -> B, B -> C
    // Verify C appears in resolution
    let mut reg = MockRegistry::new();
    reg.add_version_with_extras(
        "a",
        "1.0.0",
        vec![],
        HashMap::from([("extra1".to_string(), vec![("b", ">=1.0")])]),
    )
    .add_version("b", "1.0.0", vec![("c", ">=1.0")])
    .add_version("c", "1.0.0", vec![]);

    let result = resolve(
        reg,
        cfg("3.11"),
        vec![r_extra("a", vec!["extra1"], ">=1.0")],
    )
    .unwrap();

    assert_version(&result, "a", "1.0.0");
    assert_version(&result, "b", "1.0.0");
    assert_version(&result, "c", "1.0.0");
}

// ── Scenario: Multiple extras on same package ──────────────────────

#[test]
fn multiple_extras_on_same_package() {
    // A[extra1,extra2] where extra1 -> B, extra2 -> C
    // Verify both B and C are in resolution
    let mut reg = MockRegistry::new();
    reg.add_version_with_extras(
        "a",
        "1.0.0",
        vec![],
        HashMap::from([
            ("extra1".to_string(), vec![("b", ">=1.0")]),
            ("extra2".to_string(), vec![("c", ">=1.0")]),
        ]),
    )
    .add_version("b", "2.0.0", vec![])
    .add_version("c", "3.0.0", vec![]);

    let result = resolve(
        reg,
        cfg("3.11"),
        vec![r_extra("a", vec!["extra1", "extra2"], ">=1.0")],
    )
    .unwrap();

    assert_version(&result, "a", "1.0.0");
    assert_version(&result, "b", "2.0.0");
    assert_version(&result, "c", "3.0.0");
}

// ── Scenario: Pre-release version selection with Allow policy ──────

#[test]
fn prerelease_selected_when_only_option_with_allow() {
    // Package has only pre-release versions: 1.0.0a1, 1.0.0b1, 1.0.0rc1
    // With PreReleasePolicy::Allow, should pick the highest pre-release
    let mut reg = MockRegistry::new();
    reg.add_version("pkg", "1.0.0a1", vec![])
        .add_version("pkg", "1.0.0b1", vec![])
        .add_version("pkg", "1.0.0rc1", vec![]);

    let result = resolve(reg, cfg_pre("3.11"), vec![r("pkg", ">=1.0.0a1")]).unwrap();
    assert_version(&result, "pkg", "1.0.0rc1");
}

// ── Scenario: Yanked version avoidance ─────────────────────────────

#[test]
fn yanked_version_skipped_for_older_non_yanked() {
    // pkg has 1.0.0 (ok), 2.0.0 (yanked), 3.0.0 (yanked)
    // Should pick 1.0.0
    let mut reg = MockRegistry::new();
    reg.add_version("pkg", "1.0.0", vec![])
        .add_yanked_version("pkg", "2.0.0", vec![])
        .add_yanked_version("pkg", "3.0.0", vec![]);

    let result = resolve(reg, cfg("3.11"), vec![r("pkg", ">=1.0")]).unwrap();
    assert_version(&result, "pkg", "1.0.0");
}
