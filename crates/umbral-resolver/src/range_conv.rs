//! Conversion from PEP 440 version specifiers to pubgrub `Range`s.
//!
//! This is the bridge between Python's version constraint syntax (`>=1.0, <2.0`)
//! and pubgrub's set-theoretic version ranges.

use pubgrub::range::Range;
use pubgrub::version::Version as PubgrubVersion;
use umbral_pep440::{Operator, Version, VersionSpecifier, VersionSpecifiers};

use crate::version::UmbralVersion;

/// Convert PEP 440 `VersionSpecifiers` (comma-separated, AND-combined) to a pubgrub `Range`.
pub fn specifiers_to_range(specifiers: &VersionSpecifiers) -> Range<UmbralVersion> {
    if specifiers.is_empty() {
        return Range::any();
    }

    specifiers
        .0
        .iter()
        .map(specifier_to_range)
        .fold(Range::any(), |acc, r| acc.intersection(&r))
}

/// Convert a single PEP 440 `VersionSpecifier` to a pubgrub `Range`.
fn specifier_to_range(spec: &VersionSpecifier) -> Range<UmbralVersion> {
    let Some(ref version) = spec.version else {
        // ArbitraryEqual (`===`) without a parseable version — no match
        return Range::none();
    };

    match spec.op {
        Operator::GreaterEqual => Range::higher_than(UmbralVersion::new(version.clone())),

        Operator::Greater => {
            // Strictly greater: use bump() so the boundary version is excluded.
            // Since real versions have bump_count=0 and any v > version has
            // inner(v) > inner(version), v(0) > version(1).
            let v = UmbralVersion::new(version.clone());
            Range::higher_than(v.bump())
        }

        Operator::LessEqual => {
            // Include the boundary: [lowest, version.bump()) contains version itself
            // because version(0) < version(1).
            let v = UmbralVersion::new(version.clone());
            Range::strictly_lower_than(v.bump())
        }

        Operator::Less => Range::strictly_lower_than(UmbralVersion::new(version.clone())),

        Operator::Equal => {
            if spec.wildcard {
                wildcard_range(version.epoch, &version.release)
            } else {
                Range::exact(UmbralVersion::new(version.clone()))
            }
        }

        Operator::NotEqual => {
            if spec.wildcard {
                wildcard_range(version.epoch, &version.release).negate()
            } else {
                Range::exact(UmbralVersion::new(version.clone())).negate()
            }
        }

        Operator::Compatible => compatible_range(version),

        Operator::ArbitraryEqual => {
            // Best effort: exact match on the parsed version
            Range::exact(UmbralVersion::new(version.clone()))
        }
    }
}

/// Create a range for wildcard matching: `==X.Y.*` → `[X.Y.dev0, X.(Y+1).dev0)`.
///
/// The lower bound is the lowest possible version with the given release prefix
/// (i.e., with `.dev0`), and the upper bound is the lowest version with the
/// incremented prefix. This correctly includes pre-releases of matching versions.
///
/// The `epoch` parameter is passed through to the generated bounds so that
/// specifiers like `==1!1.0.*` produce bounds with epoch 1 rather than 0.
fn wildcard_range(epoch: u64, release_prefix: &[u64]) -> Range<UmbralVersion> {
    let lower = lowest_version_with_release(epoch, release_prefix.to_vec());
    let upper_release = increment_last_segment(release_prefix);
    let upper = lowest_version_with_release(epoch, upper_release);
    Range::between(UmbralVersion::new(lower), UmbralVersion::new(upper))
}

/// Create a range for compatible release: `~=X.Y.Z` → `[X.Y.Z, X.(Y+1).dev0)`.
///
/// Equivalent to `>=X.Y.Z, ==X.Y.*` — at least the specified version, but within
/// the same minor (or major, for two-segment specifiers) release.
fn compatible_range(version: &Version) -> Range<UmbralVersion> {
    if version.release.len() < 2 {
        // `~=` requires at least N.N — invalid, treat as unrestricted
        return Range::any();
    }

    let lower = UmbralVersion::new(version.clone());
    let upper_prefix = &version.release[..version.release.len() - 1];
    let upper_release = increment_last_segment(upper_prefix);
    let upper = lowest_version_with_release(version.epoch, upper_release);

    Range::higher_than(lower).intersection(&Range::strictly_lower_than(UmbralVersion::new(upper)))
}

/// Create the lowest possible PEP 440 version with the given release segments
/// and epoch. This is the `.dev0` variant (e.g., `1.0.dev0`), which sorts
/// before any stable or pre-release version with the same release prefix.
///
/// The `epoch` is propagated from the original specifier so that epoch-bearing
/// specifiers like `==1!1.0.*` produce correctly-bounded ranges.
fn lowest_version_with_release(epoch: u64, release: Vec<u64>) -> Version {
    Version {
        epoch,
        release,
        pre: None,
        post: None,
        dev: Some(0),
        local: vec![],
    }
}

/// Increment the last segment of a release array.
/// `[1, 0]` → `[1, 1]`, `[1, 4, 2]` → `[1, 4, 3]`
fn increment_last_segment(release: &[u64]) -> Vec<u64> {
    let mut result = release.to_vec();
    if let Some(last) = result.last_mut() {
        *last += 1;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> UmbralVersion {
        UmbralVersion::new(s.parse::<Version>().unwrap())
    }

    fn specs(s: &str) -> VersionSpecifiers {
        s.parse().unwrap()
    }

    #[test]
    fn test_greater_equal() {
        let range = specifiers_to_range(&specs(">=1.0"));
        assert!(range.contains(&v("1.0")));
        assert!(range.contains(&v("2.0")));
        assert!(!range.contains(&v("0.9")));
    }

    #[test]
    fn test_greater() {
        let range = specifiers_to_range(&specs(">1.0"));
        assert!(!range.contains(&v("1.0")));
        assert!(range.contains(&v("1.0.1")));
        assert!(range.contains(&v("1.0.post1")));
        assert!(range.contains(&v("2.0")));
    }

    #[test]
    fn test_less_equal() {
        let range = specifiers_to_range(&specs("<=2.0"));
        assert!(range.contains(&v("2.0")));
        assert!(range.contains(&v("1.9")));
        assert!(!range.contains(&v("2.0.1")));
        assert!(!range.contains(&v("2.0.post1")));
    }

    #[test]
    fn test_less() {
        let range = specifiers_to_range(&specs("<2.0"));
        assert!(!range.contains(&v("2.0")));
        assert!(range.contains(&v("1.9")));
        assert!(range.contains(&v("1.99.99")));
    }

    #[test]
    fn test_exact() {
        let range = specifiers_to_range(&specs("==1.0"));
        assert!(range.contains(&v("1.0")));
        assert!(range.contains(&v("1.0.0"))); // 1.0 == 1.0.0 in PEP 440
        assert!(!range.contains(&v("1.0.1")));
        assert!(!range.contains(&v("1.0.post1")));
    }

    #[test]
    fn test_not_equal() {
        let range = specifiers_to_range(&specs("!=1.0"));
        assert!(!range.contains(&v("1.0")));
        assert!(range.contains(&v("1.0.1")));
        assert!(range.contains(&v("0.9")));
    }

    #[test]
    fn test_wildcard_equal() {
        let range = specifiers_to_range(&specs("==1.0.*"));
        assert!(range.contains(&v("1.0.0")));
        assert!(range.contains(&v("1.0.5")));
        assert!(range.contains(&v("1.0.99")));
        assert!(!range.contains(&v("1.1.0")));
        assert!(!range.contains(&v("0.9.0")));
    }

    #[test]
    fn test_compatible_release() {
        // ~=1.4.2 is >=1.4.2, ==1.4.*
        let range = specifiers_to_range(&specs("~=1.4.2"));
        assert!(range.contains(&v("1.4.2")));
        assert!(range.contains(&v("1.4.5")));
        assert!(!range.contains(&v("1.4.1")));
        assert!(!range.contains(&v("1.5.0")));
    }

    #[test]
    fn test_combined_specifiers() {
        let range = specifiers_to_range(&specs(">=1.0, <2.0"));
        assert!(range.contains(&v("1.0")));
        assert!(range.contains(&v("1.5")));
        assert!(!range.contains(&v("0.9")));
        assert!(!range.contains(&v("2.0")));
    }

    #[test]
    fn test_empty_specifiers() {
        let range = specifiers_to_range(&specs(""));
        assert!(range.contains(&v("0.0.1")));
        assert!(range.contains(&v("999.0")));
    }

    #[test]
    fn test_python_version_range() {
        // Simulating requires-python: >=3.9
        let range = specifiers_to_range(&specs(">=3.9"));
        assert!(range.contains(&v("3.9")));
        assert!(range.contains(&v("3.11")));
        assert!(range.contains(&v("3.12")));
        assert!(!range.contains(&v("3.8")));
    }

    // ── Epoch handling (Fix 3) ──────────────────────────────────────

    #[test]
    fn test_wildcard_with_epoch() {
        // ==1!1.0.* should match 1!1.0.x but NOT 0!1.0.x or 1!1.1.x
        let range = specifiers_to_range(&specs("==1!1.0.*"));
        assert!(range.contains(&v("1!1.0.0")));
        assert!(range.contains(&v("1!1.0.5")));
        assert!(range.contains(&v("1!1.0.99")));
        assert!(!range.contains(&v("1.0.0"))); // epoch 0
        assert!(!range.contains(&v("1!1.1.0"))); // wrong minor
    }

    #[test]
    fn test_compatible_release_with_epoch() {
        // ~=1!1.4.2 is >=1!1.4.2, ==1!1.4.*
        let range = specifiers_to_range(&specs("~=1!1.4.2"));
        assert!(range.contains(&v("1!1.4.2")));
        assert!(range.contains(&v("1!1.4.5")));
        assert!(!range.contains(&v("1!1.4.1")));
        assert!(!range.contains(&v("1!1.5.0")));
        assert!(!range.contains(&v("1.4.5"))); // epoch 0 must not match
    }

    #[test]
    fn test_not_equal_wildcard_with_epoch() {
        // !=1!2.0.* should exclude 1!2.0.x but include everything else
        let range = specifiers_to_range(&specs("!=1!2.0.*"));
        assert!(!range.contains(&v("1!2.0.0")));
        assert!(!range.contains(&v("1!2.0.5")));
        assert!(range.contains(&v("1!2.1.0")));
        assert!(range.contains(&v("2.0.0"))); // epoch 0 not excluded
    }
}
