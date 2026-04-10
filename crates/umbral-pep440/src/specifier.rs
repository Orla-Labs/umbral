//! PEP 440 version specifiers: operators and matching.
//!
//! Supports all 8 comparison operators:
//! - `~=` (compatible release)
//! - `==` (version matching, with wildcards)
//! - `!=` (version exclusion, with wildcards)
//! - `<=`, `>=`, `<`, `>` (ordered comparison)
//! - `===` (arbitrary equality)

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::version::{ParseError, Version};

/// A comparison operator in a version specifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Operator {
    /// `~=` — Compatible release
    Compatible,
    /// `==` — Version matching (supports wildcards like `==1.0.*`)
    Equal,
    /// `!=` — Version exclusion (supports wildcards like `!=1.0.*`)
    NotEqual,
    /// `<=`
    LessEqual,
    /// `>=`
    GreaterEqual,
    /// `<`
    Less,
    /// `>`
    Greater,
    /// `===` — Arbitrary equality (string comparison)
    ArbitraryEqual,
}

impl fmt::Display for Operator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Operator::Compatible => write!(f, "~="),
            Operator::Equal => write!(f, "=="),
            Operator::NotEqual => write!(f, "!="),
            Operator::LessEqual => write!(f, "<="),
            Operator::GreaterEqual => write!(f, ">="),
            Operator::Less => write!(f, "<"),
            Operator::Greater => write!(f, ">"),
            Operator::ArbitraryEqual => write!(f, "==="),
        }
    }
}

/// A single version specifier, e.g., `>=1.0` or `==1.0.*`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VersionSpecifier {
    pub op: Operator,
    /// The version string (raw, for wildcard matching).
    pub version_str: String,
    /// The parsed version (None for wildcard-only like `==*`).
    pub version: Option<Version>,
    /// Whether this specifier uses a wildcard (e.g., `==1.0.*`).
    pub wildcard: bool,
}

impl VersionSpecifier {
    /// Check if a given version matches this specifier.
    pub fn contains(&self, version: &Version) -> bool {
        // For specifier matching, ignore local version of the candidate
        // unless the specifier itself has a local version.
        let candidate = if self.version.as_ref().is_none_or(|v| v.local.is_empty()) {
            version.without_local()
        } else {
            version.clone()
        };

        match self.op {
            Operator::Compatible => self.matches_compatible(&candidate),
            Operator::Equal => self.matches_equal(&candidate),
            Operator::NotEqual => !self.matches_equal(&candidate),
            Operator::LessEqual => {
                if let Some(ref spec_v) = self.version {
                    candidate <= *spec_v
                } else {
                    false
                }
            }
            Operator::GreaterEqual => {
                if let Some(ref spec_v) = self.version {
                    candidate >= *spec_v
                } else {
                    false
                }
            }
            Operator::Less => {
                if let Some(ref spec_v) = self.version {
                    candidate < *spec_v
                } else {
                    false
                }
            }
            Operator::Greater => {
                if let Some(ref spec_v) = self.version {
                    candidate > *spec_v
                } else {
                    false
                }
            }
            Operator::ArbitraryEqual => {
                // String comparison
                version.to_string() == self.version_str
            }
        }
    }

    /// `~=V.N` is equivalent to `>=V.N, ==V.*`
    fn matches_compatible(&self, candidate: &Version) -> bool {
        let Some(ref spec_v) = self.version else {
            return false;
        };

        // Must be >= the specified version
        if candidate < spec_v {
            return false;
        }

        // Must match the prefix: all release segments except the last
        if spec_v.release.len() < 2 {
            return false; // ~= requires at least N.N
        }

        let prefix_len = spec_v.release.len() - 1;
        let spec_prefix = &spec_v.release[..prefix_len];
        let cand_padded = candidate.release_padded(prefix_len);
        let cand_prefix = &cand_padded[..prefix_len];

        // Epochs must match
        if candidate.epoch != spec_v.epoch {
            return false;
        }

        spec_prefix == cand_prefix
    }

    /// `==V` or `==V.*` — version matching.
    fn matches_equal(&self, candidate: &Version) -> bool {
        let Some(ref spec_v) = self.version else {
            return false;
        };

        // Epochs must match
        if candidate.epoch != spec_v.epoch {
            return false;
        }

        if self.wildcard {
            // Wildcard matching: prefix match on release segments
            let prefix_len = spec_v.release.len();
            let cand_padded = candidate.release_padded(prefix_len);
            cand_padded[..prefix_len] == spec_v.release[..]
        } else {
            // Exact match (ignoring local unless spec has local)
            *candidate == *spec_v
        }
    }
}

impl fmt::Display for VersionSpecifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.op, self.version_str)
    }
}

impl FromStr for VersionSpecifier {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();

        // Parse operator
        let (op, rest) = if let Some(rest) = s.strip_prefix("===") {
            (Operator::ArbitraryEqual, rest)
        } else if let Some(rest) = s.strip_prefix("~=") {
            (Operator::Compatible, rest)
        } else if let Some(rest) = s.strip_prefix("==") {
            (Operator::Equal, rest)
        } else if let Some(rest) = s.strip_prefix("!=") {
            (Operator::NotEqual, rest)
        } else if let Some(rest) = s.strip_prefix("<=") {
            (Operator::LessEqual, rest)
        } else if let Some(rest) = s.strip_prefix(">=") {
            (Operator::GreaterEqual, rest)
        } else if let Some(rest) = s.strip_prefix('<') {
            (Operator::Less, rest)
        } else if let Some(rest) = s.strip_prefix('>') {
            (Operator::Greater, rest)
        } else {
            return Err(ParseError::InvalidVersion(format!(
                "no operator found in: {s}"
            )));
        };

        let rest = rest.trim();
        let wildcard = rest.ends_with(".*");
        let version_str = rest.to_string();

        // PEP 440: wildcards are only valid with == and != operators.
        if wildcard && !matches!(op, Operator::Equal | Operator::NotEqual) {
            return Err(ParseError::InvalidVersion(format!(
                "wildcard version '{}' not allowed with operator '{}'",
                version_str, op
            )));
        }

        let version = if op == Operator::ArbitraryEqual {
            None
        } else if wildcard {
            // Parse the non-wildcard prefix
            let prefix = &rest[..rest.len() - 2]; // strip ".*"
            Some(prefix.parse::<Version>()?)
        } else {
            Some(rest.parse::<Version>()?)
        };

        // PEP 440: ~= (compatible release) requires at least two release segments.
        if op == Operator::Compatible {
            if let Some(ref v) = version {
                if v.release.len() < 2 {
                    return Err(ParseError::InvalidVersion(format!(
                        "~= operator requires a version with at least two release segments, got: {}",
                        v
                    )));
                }
            }
        }

        Ok(VersionSpecifier {
            op,
            version_str,
            version,
            wildcard,
        })
    }
}

/// A comma-separated list of version specifiers (AND-combined).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VersionSpecifiers(pub Vec<VersionSpecifier>);

impl VersionSpecifiers {
    /// Check if a version satisfies ALL specifiers.
    pub fn contains(&self, version: &Version) -> bool {
        self.0.iter().all(|spec| spec.contains(version))
    }

    /// Returns true if this set of specifiers is empty (matches anything).
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for VersionSpecifiers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = self.0.iter().map(|s| s.to_string()).collect();
        write!(f, "{}", parts.join(", "))
    }
}

impl FromStr for VersionSpecifiers {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Ok(VersionSpecifiers(Vec::new()));
        }

        let specifiers: Result<Vec<VersionSpecifier>, _> =
            s.split(',').map(|part| part.trim().parse()).collect();
        Ok(VersionSpecifiers(specifiers?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    fn spec(s: &str) -> VersionSpecifier {
        s.parse().unwrap()
    }

    // -- Operator parsing --

    #[test]
    fn test_parse_operators() {
        assert_eq!(spec(">=1.0").op, Operator::GreaterEqual);
        assert_eq!(spec("<=1.0").op, Operator::LessEqual);
        assert_eq!(spec(">1.0").op, Operator::Greater);
        assert_eq!(spec("<1.0").op, Operator::Less);
        assert_eq!(spec("==1.0").op, Operator::Equal);
        assert_eq!(spec("!=1.0").op, Operator::NotEqual);
        assert_eq!(spec("~=1.0").op, Operator::Compatible);
        assert_eq!(spec("===1.0").op, Operator::ArbitraryEqual);
    }

    // -- Greater/Less --

    #[test]
    fn test_greater_equal() {
        let s = spec(">=1.5");
        assert!(s.contains(&v("1.5")));
        assert!(s.contains(&v("2.0")));
        assert!(!s.contains(&v("1.4")));
    }

    #[test]
    fn test_less_than() {
        let s = spec("<2.0");
        assert!(s.contains(&v("1.9")));
        assert!(!s.contains(&v("2.0")));
        assert!(!s.contains(&v("2.1")));
    }

    // -- Equal with wildcards --

    #[test]
    fn test_equal_exact() {
        let s = spec("==1.0.0");
        assert!(s.contains(&v("1.0.0")));
        assert!(!s.contains(&v("1.0.1")));
    }

    #[test]
    fn test_equal_wildcard() {
        let s = spec("==1.0.*");
        assert!(s.contains(&v("1.0.0")));
        assert!(s.contains(&v("1.0.5")));
        assert!(s.contains(&v("1.0.99")));
        assert!(!s.contains(&v("1.1.0")));
        assert!(!s.contains(&v("2.0.0")));
    }

    // -- Not equal --

    #[test]
    fn test_not_equal() {
        let s = spec("!=1.0.0");
        assert!(!s.contains(&v("1.0.0")));
        assert!(s.contains(&v("1.0.1")));
    }

    // -- Compatible release --

    #[test]
    fn test_compatible_release() {
        // ~=1.4.2 is equivalent to >=1.4.2, ==1.4.*
        let s = spec("~=1.4.2");
        assert!(s.contains(&v("1.4.2")));
        assert!(s.contains(&v("1.4.5")));
        assert!(!s.contains(&v("1.4.1"))); // less than
        assert!(!s.contains(&v("1.5.0"))); // different prefix
        assert!(!s.contains(&v("2.0.0")));
    }

    #[test]
    fn test_compatible_release_two_segments() {
        // ~=1.4 is equivalent to >=1.4, ==1.*
        let s = spec("~=1.4");
        assert!(s.contains(&v("1.4")));
        assert!(s.contains(&v("1.5")));
        assert!(s.contains(&v("1.99")));
        assert!(!s.contains(&v("1.3")));
        assert!(!s.contains(&v("2.0")));
    }

    // -- Combined specifiers --

    #[test]
    fn test_version_specifiers() {
        let specs: VersionSpecifiers = ">=1.0, <2.0".parse().unwrap();
        assert!(specs.contains(&v("1.0")));
        assert!(specs.contains(&v("1.5")));
        assert!(!specs.contains(&v("0.9")));
        assert!(!specs.contains(&v("2.0")));
    }

    #[test]
    fn test_version_specifiers_empty() {
        let specs: VersionSpecifiers = "".parse().unwrap();
        assert!(specs.contains(&v("1.0"))); // empty matches anything
    }

    // -- Pre-release interaction --

    #[test]
    fn test_prerelease_matching() {
        let s = spec(">=1.0a1");
        assert!(s.contains(&v("1.0a1")));
        assert!(s.contains(&v("1.0a2")));
        assert!(s.contains(&v("1.0b1")));
        assert!(s.contains(&v("1.0")));
        assert!(!s.contains(&v("1.0.dev0")));
    }

    // -- Display --

    #[test]
    fn test_display_specifier() {
        assert_eq!(spec(">=1.0").to_string(), ">=1.0");
        assert_eq!(spec("==1.0.*").to_string(), "==1.0.*");
        assert_eq!(spec("~=1.4.2").to_string(), "~=1.4.2");
    }

    #[test]
    fn test_display_specifiers() {
        let specs: VersionSpecifiers = ">=1.0, <2.0, !=1.5.0".parse().unwrap();
        assert_eq!(specs.to_string(), ">=1.0, <2.0, !=1.5.0");
    }

    // -- Wildcard rejection for invalid operators (Fix 4) --

    #[test]
    fn test_wildcard_valid_with_equal() {
        assert!("==1.0.*".parse::<VersionSpecifier>().is_ok());
    }

    #[test]
    fn test_wildcard_valid_with_not_equal() {
        assert!("!=1.0.*".parse::<VersionSpecifier>().is_ok());
    }

    #[test]
    fn test_wildcard_rejected_ge() {
        assert!(
            ">=1.0.*".parse::<VersionSpecifier>().is_err(),
            ">= with wildcard should be rejected"
        );
    }

    #[test]
    fn test_wildcard_rejected_le() {
        assert!(
            "<=1.0.*".parse::<VersionSpecifier>().is_err(),
            "<= with wildcard should be rejected"
        );
    }

    #[test]
    fn test_wildcard_rejected_lt() {
        assert!(
            "<1.0.*".parse::<VersionSpecifier>().is_err(),
            "< with wildcard should be rejected"
        );
    }

    #[test]
    fn test_wildcard_rejected_gt() {
        assert!(
            ">1.0.*".parse::<VersionSpecifier>().is_err(),
            "> with wildcard should be rejected"
        );
    }

    #[test]
    fn test_wildcard_rejected_compatible() {
        assert!(
            "~=1.0.*".parse::<VersionSpecifier>().is_err(),
            "~= with wildcard should be rejected"
        );
    }

    #[test]
    fn test_compatible_single_segment_rejected() {
        // PEP 440: ~= with a single release segment (e.g. ~=1) is invalid.
        let result = "~=1".parse::<VersionSpecifier>();
        assert!(
            result.is_err(),
            "~=1 (single segment) should be rejected at parse time"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("at least two release segments"),
            "error should mention two release segments, got: {err_msg}"
        );
    }

    // ── Edge case: Compatible release with complex version ──────

    #[test]
    fn test_compatible_release_complex_version() {
        // ~=1.4.2 should match 1.4.3 but not 1.5.0
        let s = spec("~=1.4.2");
        assert!(s.contains(&v("1.4.2")), "~=1.4.2 should match 1.4.2");
        assert!(s.contains(&v("1.4.3")), "~=1.4.2 should match 1.4.3");
        assert!(s.contains(&v("1.4.99")), "~=1.4.2 should match 1.4.99");
        assert!(
            !s.contains(&v("1.4.1")),
            "~=1.4.2 should NOT match 1.4.1 (too low)"
        );
        assert!(
            !s.contains(&v("1.5.0")),
            "~=1.4.2 should NOT match 1.5.0 (different prefix)"
        );
        assert!(!s.contains(&v("2.0.0")), "~=1.4.2 should NOT match 2.0.0");
    }

    // ── Edge case: Compatible release with 4-segment version ────

    #[test]
    fn test_compatible_release_four_segments() {
        // ~=1.4.2.0 should match >=1.4.2.0, ==1.4.2.*
        let s = spec("~=1.4.2.0");
        assert!(s.contains(&v("1.4.2.0")));
        assert!(s.contains(&v("1.4.2.1")));
        assert!(s.contains(&v("1.4.2.99")));
        assert!(
            !s.contains(&v("1.4.3.0")),
            "~=1.4.2.0 should NOT match 1.4.3.0"
        );
    }
}
