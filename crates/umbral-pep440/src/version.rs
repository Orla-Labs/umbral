//! PEP 440 Version parsing, normalization, and ordering.
//!
//! Implements the full PEP 440 version scheme including:
//! - Epoch segments (N!)
//! - Release segments (N.N.N...)
//! - Pre-release tags (aN, bN, rcN)
//! - Post-release tags (.postN)
//! - Dev-release tags (.devN)
//! - Local version labels (+local.1)

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;
use std::sync::LazyLock;

use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    #[error("invalid version: {0}")]
    InvalidVersion(String),
}

/// The kind of pre-release: alpha, beta, or release candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PreReleaseKind {
    Alpha,
    Beta,
    Rc,
}

impl PreReleaseKind {
    fn order(&self) -> u8 {
        match self {
            PreReleaseKind::Alpha => 0,
            PreReleaseKind::Beta => 1,
            PreReleaseKind::Rc => 2,
        }
    }
}

impl fmt::Display for PreReleaseKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PreReleaseKind::Alpha => write!(f, "a"),
            PreReleaseKind::Beta => write!(f, "b"),
            PreReleaseKind::Rc => write!(f, "rc"),
        }
    }
}

/// A pre-release tag: (kind, number).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PreRelease {
    pub kind: PreReleaseKind,
    pub number: u64,
}

impl fmt::Display for PreRelease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.kind, self.number)
    }
}

/// A local version segment — either numeric or string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LocalSegment {
    Number(u64),
    String(String),
}

impl fmt::Display for LocalSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LocalSegment::Number(n) => write!(f, "{n}"),
            LocalSegment::String(s) => write!(f, "{s}"),
        }
    }
}

/// A parsed, normalized PEP 440 version.
///
/// Ordering follows PEP 440 exactly:
/// - Epoch is compared first
/// - Then release segments (zero-padded on the right)
/// - Then: dev < pre < (no suffix) < post
/// - Local versions are compared only for equality/ordering, not for matching
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Version {
    pub epoch: u64,
    pub release: Vec<u64>,
    pub pre: Option<PreRelease>,
    pub post: Option<u64>,
    pub dev: Option<u64>,
    pub local: Vec<LocalSegment>,
}

impl Version {
    /// Create a new version from release segments only.
    pub fn new(release: Vec<u64>) -> Self {
        Self {
            epoch: 0,
            release,
            pre: None,
            post: None,
            dev: None,
            local: Vec::new(),
        }
    }

    /// Returns true if this is a pre-release version (has pre, dev, or both).
    pub fn is_prerelease(&self) -> bool {
        self.pre.is_some() || self.dev.is_some()
    }

    /// Returns true if this version has a local segment.
    pub fn is_local(&self) -> bool {
        !self.local.is_empty()
    }

    /// Returns the major version (first release segment, or 0).
    pub fn major(&self) -> u64 {
        self.release.first().copied().unwrap_or(0)
    }

    /// Returns the minor version (second release segment, or 0).
    pub fn minor(&self) -> u64 {
        self.release.get(1).copied().unwrap_or(0)
    }

    /// Returns the micro/patch version (third release segment, or 0).
    pub fn micro(&self) -> u64 {
        self.release.get(2).copied().unwrap_or(0)
    }

    /// Strip local version for comparison in specifiers.
    pub fn without_local(&self) -> Version {
        Version {
            epoch: self.epoch,
            release: self.release.clone(),
            pre: self.pre,
            post: self.post,
            dev: self.dev,
            local: Vec::new(),
        }
    }

    /// Get the release segments zero-padded to a given length.
    pub(crate) fn release_padded(&self, len: usize) -> Vec<u64> {
        let mut r = self.release.clone();
        r.resize(len, 0);
        r
    }

    /// Returns a sort key tuple following PEP 440 ordering.
    ///
    /// The ordering is:
    /// X.Y.devN < X.YaN.devM < X.YaN < X.YaN.postM < ... < X.Y < X.Y.postN.devM < X.Y.postN
    ///
    /// We produce a multi-level key:
    /// (pre_phase, pre_kind_order, pre_number, post_phase, suffix_phase, suffix_num, suffix_dev)
    ///
    /// pre_phase: -2 = dev-only, -1 = has pre-release, 0 = final, 1 = post-only
    /// Within pre-release: ordered by (kind, number), then dev < no-suffix < post
    ///
    /// The suffix_dev field differentiates post.devN values (e.g. post5.dev0 vs post5.dev999).
    /// For post/pre.post with dev: suffix_num = post*2, suffix_dev = dev number.
    /// For post/pre.post without dev: suffix_num = post*2+1, suffix_dev = 0.
    fn suffix_key(&self) -> impl Ord {
        // Level 1: pre-release kind and number (if any)
        let (pre_phase, pre_kind, pre_num): (i8, i8, u64) = match &self.pre {
            Some(pre) => (-1, pre.kind.order() as i8, pre.number),
            None => {
                if self.dev.is_some() && self.post.is_none() {
                    // dev-only (no pre, no post): comes before any pre-release
                    (-2, 0, 0)
                } else {
                    (0, 0, 0) // final or post
                }
            }
        };

        // Level 2: within same pre-release, ordering is dev < (none) < post
        // And within dev or post, by their number.
        // The third element (suffix_dev) captures the dev number to avoid lossy encoding.
        let (suffix_phase, suffix_num, suffix_dev): (i8, u64, u64) = if pre_phase == -2 {
            // dev-only: just use dev number
            (0, self.dev.unwrap_or(0), 0)
        } else if self.pre.is_some() {
            // Has pre-release: check for .devN or .postN suffix on the pre
            // Ordering: pre.devN < pre < pre.postN.devM < pre.postN
            match (&self.dev, &self.post) {
                (Some(dev), None) => (-1, *dev, 0), // pre.devN (before the pre itself)
                (None, None) => (0, 0, 0),          // just pre
                (Some(dev), Some(post)) => {
                    // pre.postN.devM (after pre, before pre.postN)
                    (1, *post * 2, *dev) // even = has dev, dev number differentiates
                }
                (None, Some(post)) => (1, *post * 2 + 1, 0), // pre.postN (after pre.postN.devM)
            }
        } else {
            // Final or post release
            match (&self.dev, &self.post) {
                (None, None) => (0, 0, 0),                        // final
                (Some(dev), Some(post)) => (-1, *post * 2, *dev), // post.devN (before post)
                (None, Some(post)) => (1, *post * 2 + 1, 0),      // post (odd = no dev)
                (Some(dev), None) => (-2, *dev, 0),               // shouldn't happen but handle
            }
        };

        // Final post-phase adjustment: final < post
        let post_phase: i8 = if pre_phase == 0 && self.post.is_some() {
            1
        } else {
            0
        };

        (
            pre_phase,
            pre_kind,
            pre_num,
            post_phase,
            suffix_phase,
            suffix_num,
            suffix_dev,
        )
    }
}

// -- Display (canonical form) --

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Epoch
        if self.epoch != 0 {
            write!(f, "{}!", self.epoch)?;
        }

        // Release segments
        let release_str: Vec<String> = self.release.iter().map(|s| s.to_string()).collect();
        write!(f, "{}", release_str.join("."))?;

        // Pre-release
        if let Some(ref pre) = self.pre {
            write!(f, "{pre}")?;
        }

        // Post-release
        if let Some(post) = self.post {
            write!(f, ".post{post}")?;
        }

        // Dev release
        if let Some(dev) = self.dev {
            write!(f, ".dev{dev}")?;
        }

        // Local
        if !self.local.is_empty() {
            write!(f, "+")?;
            let parts: Vec<String> = self.local.iter().map(|s| s.to_string()).collect();
            write!(f, "{}", parts.join("."))?;
        }

        Ok(())
    }
}

// -- Equality: ignores local segments per PEP 440 --

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

impl Hash for Version {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.epoch.hash(state);
        // Trim trailing zeros for consistent hashing (1.0 == 1.0.0)
        let trimmed = self.release.iter().rev().skip_while(|&&v| v == 0).count();
        let effective_len = trimmed.max(1);
        self.release[..effective_len].hash(state);
        self.pre.hash(state);
        self.post.hash(state);
        self.dev.hash(state);
        // Normalize local segments for hash consistency with case-insensitive Eq
        for seg in &self.local {
            match seg {
                LocalSegment::Number(n) => {
                    0u8.hash(state);
                    n.hash(state);
                }
                LocalSegment::String(s) => {
                    1u8.hash(state);
                    s.to_lowercase().hash(state);
                }
            }
        }
    }
}

// -- Ordering: full PEP 440 --

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        // 1. Epoch
        let epoch_cmp = self.epoch.cmp(&other.epoch);
        if epoch_cmp != Ordering::Equal {
            return epoch_cmp;
        }

        // 2. Release segments (zero-padded)
        let max_len = self.release.len().max(other.release.len());
        let self_rel = self.release_padded(max_len);
        let other_rel = other.release_padded(max_len);
        let rel_cmp = self_rel.cmp(&other_rel);
        if rel_cmp != Ordering::Equal {
            return rel_cmp;
        }

        // 3. Suffix (dev/pre/post)
        let self_suffix = self.suffix_key();
        let other_suffix = other.suffix_key();
        let suffix_cmp = self_suffix.cmp(&other_suffix);
        if suffix_cmp != Ordering::Equal {
            return suffix_cmp;
        }

        // 4. Local version segments
        // Versions without local < versions with local
        match (self.local.is_empty(), other.local.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => {
                let max_local = self.local.len().max(other.local.len());
                for i in 0..max_local {
                    let a = self.local.get(i);
                    let b = other.local.get(i);
                    match (a, b) {
                        (None, Some(_)) => return Ordering::Less,
                        (Some(_), None) => return Ordering::Greater,
                        (Some(LocalSegment::Number(x)), Some(LocalSegment::Number(y))) => {
                            let c = x.cmp(y);
                            if c != Ordering::Equal {
                                return c;
                            }
                        }
                        (Some(LocalSegment::String(x)), Some(LocalSegment::String(y))) => {
                            let c = x.to_lowercase().cmp(&y.to_lowercase());
                            if c != Ordering::Equal {
                                return c;
                            }
                        }
                        // Per PEP 440 / packaging: numeric segments sort before strings
                        (Some(LocalSegment::Number(_)), Some(LocalSegment::String(_))) => {
                            return Ordering::Less;
                        }
                        (Some(LocalSegment::String(_)), Some(LocalSegment::Number(_))) => {
                            return Ordering::Greater;
                        }
                        (None, None) => unreachable!(),
                    }
                }
                Ordering::Equal
            }
        }
    }
}

// -- Parsing --

/// PEP 440 version regex (permissive, handles all normalization forms).
static VERSION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?xi)
        ^
        \s*
        v?                                           # optional 'v' prefix
        (?:(?P<epoch>[0-9]+)!)?                      # epoch
        (?P<release>[0-9]+(?:\.[0-9]+)*)             # release segment
        (?:                                          # pre-release
            [-_.]?
            (?P<pre_kind>alpha|a|beta|b|preview|c|rc)
            [-_.]?
            (?P<pre_num>[0-9]+)?
        )?
        (?:                                          # post release
            (?:
                [-_.]?
                (?P<post_kw>post|rev|r)
                [-_.]?
                (?P<post_num1>[0-9]+)?
            )
            |
            (?:
                -(?P<post_num2>[0-9]+)               # implicit post (e.g., 1.0-1)
            )
        )?
        (?:                                          # dev release
            [-_.]?
            (?P<dev_kw>dev)
            [-_.]?
            (?P<dev_num>[0-9]+)?
        )?
        (?:\+(?P<local>[a-z0-9](?:[a-z0-9._-]*[a-z0-9])?))?  # local version
        \s*
        $
    ",
    )
    .expect("VERSION_RE is a valid regex")
});

impl FromStr for Version {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let caps = VERSION_RE
            .captures(s)
            .ok_or_else(|| ParseError::InvalidVersion(s.to_string()))?;

        // Epoch
        let epoch = caps
            .name("epoch")
            .map(|m| m.as_str().parse::<u64>().expect("regex guarantees digits"))
            .unwrap_or(0);

        // Release segments
        let release: Vec<u64> = caps["release"]
            .split('.')
            .map(|s| s.parse::<u64>().expect("regex guarantees digits"))
            .collect();

        // Pre-release
        let pre = caps.name("pre_kind").map(|kind_match| {
            let kind = match kind_match.as_str().to_lowercase().as_str() {
                "a" | "alpha" => PreReleaseKind::Alpha,
                "b" | "beta" => PreReleaseKind::Beta,
                "c" | "rc" | "preview" => PreReleaseKind::Rc,
                _ => unreachable!(),
            };
            let number = caps
                .name("pre_num")
                .map(|m| m.as_str().parse::<u64>().expect("regex guarantees digits"))
                .unwrap_or(0);
            PreRelease { kind, number }
        });

        // Post-release: use the named capture group `post_kw` to detect
        // whether the post keyword actually matched in the regex, avoiding
        // false positives from local segments like `1.0+postfix`.
        let post = caps
            .name("post_num1")
            .or_else(|| caps.name("post_num2"))
            .map(|m| m.as_str().parse::<u64>().expect("regex guarantees digits"))
            .or_else(|| {
                // The keyword matched without a trailing number → default to 0
                if caps.name("post_kw").is_some() {
                    Some(0)
                } else {
                    None
                }
            });

        // Dev release: use the named capture group `dev_kw` to detect
        // whether dev actually matched, avoiding false positives from
        // local segments like `1.0+devtools`.
        let dev = if caps.name("dev_num").is_some() {
            Some(
                caps["dev_num"]
                    .parse::<u64>()
                    .expect("regex guarantees digits"),
            )
        } else if caps.name("dev_kw").is_some() {
            Some(0)
        } else {
            None
        };

        // Local version
        let local = caps
            .name("local")
            .map(|m| {
                m.as_str()
                    .split(&['.', '-', '_'][..])
                    .map(|seg| {
                        if let Ok(n) = seg.parse::<u64>() {
                            LocalSegment::Number(n)
                        } else {
                            LocalSegment::String(seg.to_lowercase())
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(Version {
            epoch,
            release,
            pre,
            post,
            dev,
            local,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple() {
        let v: Version = "1.0.0".parse().unwrap();
        assert_eq!(v.epoch, 0);
        assert_eq!(v.release, vec![1, 0, 0]);
        assert_eq!(v.pre, None);
        assert_eq!(v.post, None);
        assert_eq!(v.dev, None);
        assert!(v.local.is_empty());
    }

    #[test]
    fn test_parse_epoch() {
        let v: Version = "2!1.0".parse().unwrap();
        assert_eq!(v.epoch, 2);
        assert_eq!(v.release, vec![1, 0]);
    }

    #[test]
    fn test_parse_pre_release() {
        let v: Version = "1.0a1".parse().unwrap();
        assert_eq!(
            v.pre,
            Some(PreRelease {
                kind: PreReleaseKind::Alpha,
                number: 1
            })
        );

        let v: Version = "1.0b2".parse().unwrap();
        assert_eq!(
            v.pre,
            Some(PreRelease {
                kind: PreReleaseKind::Beta,
                number: 2
            })
        );

        let v: Version = "1.0rc1".parse().unwrap();
        assert_eq!(
            v.pre,
            Some(PreRelease {
                kind: PreReleaseKind::Rc,
                number: 1
            })
        );
    }

    #[test]
    fn test_parse_post_release() {
        let v: Version = "1.0.post1".parse().unwrap();
        assert_eq!(v.post, Some(1));
    }

    #[test]
    fn test_parse_dev_release() {
        let v: Version = "1.0.dev3".parse().unwrap();
        assert_eq!(v.dev, Some(3));
    }

    #[test]
    fn test_parse_local() {
        let v: Version = "1.0+local.1".parse().unwrap();
        assert_eq!(
            v.local,
            vec![
                LocalSegment::String("local".to_string()),
                LocalSegment::Number(1)
            ]
        );
    }

    #[test]
    fn test_parse_complex() {
        let v: Version = "1!2.3.4a5.post6.dev7+local.8".parse().unwrap();
        assert_eq!(v.epoch, 1);
        assert_eq!(v.release, vec![2, 3, 4]);
        assert_eq!(
            v.pre,
            Some(PreRelease {
                kind: PreReleaseKind::Alpha,
                number: 5
            })
        );
        assert_eq!(v.post, Some(6));
        assert_eq!(v.dev, Some(7));
        assert!(!v.local.is_empty());
    }

    #[test]
    fn test_normalization_v_prefix() {
        let v: Version = "v1.0".parse().unwrap();
        assert_eq!(v.to_string(), "1.0");
    }

    #[test]
    fn test_normalization_pre_release_spelling() {
        // "alpha" normalizes to "a"
        let v1: Version = "1.0alpha1".parse().unwrap();
        let v2: Version = "1.0a1".parse().unwrap();
        assert_eq!(v1, v2);
        assert_eq!(v1.to_string(), "1.0a1");

        // "beta" normalizes to "b"
        let v1: Version = "1.0beta2".parse().unwrap();
        let v2: Version = "1.0b2".parse().unwrap();
        assert_eq!(v1, v2);

        // "c" and "preview" normalize to "rc"
        let v1: Version = "1.0c1".parse().unwrap();
        let v2: Version = "1.0rc1".parse().unwrap();
        assert_eq!(v1, v2);

        let v1: Version = "1.0preview1".parse().unwrap();
        let v2: Version = "1.0rc1".parse().unwrap();
        assert_eq!(v1, v2);
    }

    #[test]
    fn test_display_canonical() {
        let cases = vec![
            ("1.0.0", "1.0.0"),
            ("1.0a1", "1.0a1"),
            ("1.0b2", "1.0b2"),
            ("1.0rc1", "1.0rc1"),
            ("1.0.post1", "1.0.post1"),
            ("1.0.dev0", "1.0.dev0"),
            ("2!1.0", "2!1.0"),
            ("1.0+local.1", "1.0+local.1"),
        ];

        for (input, expected) in cases {
            let v: Version = input.parse().unwrap();
            assert_eq!(v.to_string(), expected, "canonical form of {input}");
        }
    }

    // -- Ordering tests --

    #[test]
    fn test_ordering_basic() {
        let v1: Version = "1.0".parse().unwrap();
        let v2: Version = "2.0".parse().unwrap();
        assert!(v1 < v2);
    }

    #[test]
    fn test_ordering_release_segments() {
        let v1: Version = "1.0".parse().unwrap();
        let v2: Version = "1.0.0".parse().unwrap();
        assert_eq!(v1, v2); // trailing zeros are equal
    }

    #[test]
    fn test_ordering_epoch() {
        let v1: Version = "2!1.0".parse().unwrap();
        let v2: Version = "99.0".parse().unwrap();
        assert!(v1 > v2); // epoch 2 > epoch 0
    }

    // ── Edge case: Version with epoch sorts higher than no epoch ──

    #[test]
    fn test_epoch_1_beats_999() {
        // 1!2.0 should sort higher than 999.0 (no epoch means epoch 0)
        let with_epoch: Version = "1!2.0".parse().unwrap();
        let without_epoch: Version = "999.0".parse().unwrap();
        assert!(
            with_epoch > without_epoch,
            "1!2.0 should be greater than 999.0"
        );
        assert_eq!(with_epoch.epoch, 1);
        assert_eq!(without_epoch.epoch, 0);
    }

    #[test]
    fn test_epoch_0_equal_to_no_epoch() {
        // Explicitly specifying epoch 0 should be equal to no epoch
        let with_zero_epoch: Version = "0!1.0.0".parse().unwrap();
        let without_epoch: Version = "1.0.0".parse().unwrap();
        assert_eq!(with_zero_epoch, without_epoch);
    }

    #[test]
    fn test_ordering_dev_pre_post() {
        // dev < alpha < beta < rc < final < post
        let dev: Version = "1.0.dev0".parse().unwrap();
        let alpha: Version = "1.0a0".parse().unwrap();
        let beta: Version = "1.0b0".parse().unwrap();
        let rc: Version = "1.0rc0".parse().unwrap();
        let final_v: Version = "1.0".parse().unwrap();
        let post: Version = "1.0.post0".parse().unwrap();

        assert!(dev < alpha, "dev < alpha");
        assert!(alpha < beta, "alpha < beta");
        assert!(beta < rc, "beta < rc");
        assert!(rc < final_v, "rc < final");
        assert!(final_v < post, "final < post");
    }

    #[test]
    fn test_ordering_pre_numbers() {
        let a1: Version = "1.0a1".parse().unwrap();
        let a2: Version = "1.0a2".parse().unwrap();
        assert!(a1 < a2);
    }

    #[test]
    fn test_is_prerelease() {
        assert!("1.0a1".parse::<Version>().unwrap().is_prerelease());
        assert!("1.0.dev0".parse::<Version>().unwrap().is_prerelease());
        assert!(!"1.0".parse::<Version>().unwrap().is_prerelease());
        assert!(!"1.0.post1".parse::<Version>().unwrap().is_prerelease());
    }

    #[test]
    fn test_ordering_full_sequence() {
        // The complete ordering example from PEP 440
        let versions: Vec<Version> = vec![
            "1.0.dev456",
            "1.0a1",
            "1.0a2.dev456",
            "1.0a12.dev456",
            "1.0a12",
            "1.0b1.dev456",
            "1.0b2",
            "1.0b2.post345.dev456",
            "1.0b2.post345",
            "1.0rc1.dev456",
            "1.0rc1",
            "1.0",
            "1.0.post456.dev34",
            "1.0.post456",
            "1.1.dev1",
        ]
        .into_iter()
        .map(|s| s.parse().unwrap())
        .collect();

        for i in 0..versions.len() - 1 {
            assert!(
                versions[i] < versions[i + 1],
                "{} should be < {}",
                versions[i],
                versions[i + 1]
            );
        }
    }

    #[test]
    fn test_invalid_versions() {
        assert!("".parse::<Version>().is_err());
        assert!("not_a_version".parse::<Version>().is_err());
        assert!("1.0.0.0.0.0.0invalid".parse::<Version>().is_err());
    }

    // -- Local version ordering (Fix 3): numbers sort before strings --

    #[test]
    fn test_local_number_before_string() {
        let v1: Version = "1.0+1".parse().unwrap();
        let v2: Version = "1.0+abc".parse().unwrap();
        assert!(
            v1 < v2,
            "numeric local segment should sort before string: {} vs {}",
            v1,
            v2
        );
    }

    #[test]
    fn test_local_string_after_number() {
        let v1: Version = "1.0+abc".parse().unwrap();
        let v2: Version = "1.0+1".parse().unwrap();
        assert!(
            v1 > v2,
            "string local segment should sort after number: {} vs {}",
            v1,
            v2
        );
    }

    // -- Post/dev false-positive regression (Fix 2) --

    #[test]
    fn test_local_postfix_not_post_release() {
        let v: Version = "1.0+postfix".parse().unwrap();
        assert_eq!(
            v.post, None,
            "1.0+postfix should NOT have post=Some(...), local segment contains 'post' as text"
        );
    }

    #[test]
    fn test_local_devtools_not_dev_release() {
        let v: Version = "1.0+devtools".parse().unwrap();
        assert_eq!(
            v.dev, None,
            "1.0+devtools should NOT have dev=Some(...), local segment contains 'dev' as text"
        );
    }

    #[test]
    fn test_genuine_post_still_detected() {
        let v: Version = "1.0.post2".parse().unwrap();
        assert_eq!(v.post, Some(2));
    }

    #[test]
    fn test_genuine_post_no_number() {
        let v: Version = "1.0.post".parse().unwrap();
        assert_eq!(v.post, Some(0));
    }

    #[test]
    fn test_genuine_dev_still_detected() {
        let v: Version = "1.0.dev4".parse().unwrap();
        assert_eq!(v.dev, Some(4));
    }

    #[test]
    fn test_genuine_dev_no_number() {
        let v: Version = "1.0.dev".parse().unwrap();
        assert_eq!(v.dev, Some(0));
    }

    // -- Fix 1: post.devN differentiation tests --

    #[test]
    fn test_post_dev_different_dev_numbers_pre_release() {
        // 1.0a1.post5.dev0 < 1.0a1.post5.dev999
        let v1: Version = "1.0a1.post5.dev0".parse().unwrap();
        let v2: Version = "1.0a1.post5.dev999".parse().unwrap();
        assert!(
            v1 < v2,
            "1.0a1.post5.dev0 should be less than 1.0a1.post5.dev999, but got: {} vs {}",
            v1,
            v2
        );
        assert_ne!(
            v1, v2,
            "versions differing only in post.devN must not be equal"
        );
    }

    #[test]
    fn test_post_dev_different_dev_numbers_final() {
        // 1.0.post456.dev34 < 1.0.post456.dev999
        let v1: Version = "1.0.post456.dev34".parse().unwrap();
        let v2: Version = "1.0.post456.dev999".parse().unwrap();
        assert!(
            v1 < v2,
            "1.0.post456.dev34 should be less than 1.0.post456.dev999, but got: {} vs {}",
            v1,
            v2
        );
        assert_ne!(
            v1, v2,
            "versions differing only in post.devN must not be equal"
        );
    }

    // -- Fix 3: proptest property-based tests --

    #[test]
    fn test_hash_consistency_local_case_insensitive() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        // Construct versions programmatically with uppercase local segments
        let v1 = Version {
            epoch: 0,
            release: vec![1, 0],
            pre: None,
            post: None,
            dev: None,
            local: vec![LocalSegment::String("ABC".to_string())],
        };
        let v2 = Version {
            epoch: 0,
            release: vec![1, 0],
            pre: None,
            post: None,
            dev: None,
            local: vec![LocalSegment::String("abc".to_string())],
        };

        // They must be equal (case-insensitive local comparison)
        assert_eq!(v1, v2, "1.0+ABC should equal 1.0+abc");

        // They must hash identically
        let hash = |v: &Version| {
            let mut h = DefaultHasher::new();
            v.hash(&mut h);
            h.finish()
        };
        assert_eq!(
            hash(&v1),
            hash(&v2),
            "hash(1.0+ABC) must equal hash(1.0+abc)"
        );
    }

    mod proptest_tests {
        use super::*;
        use proptest::prelude::*;

        /// Strategy to generate valid PEP 440 version strings.
        fn arb_version() -> impl Strategy<Value = Version> {
            let epoch = prop::option::of(0u64..5);
            let release = prop::collection::vec(0u64..100, 1..=4);
            let pre = prop::option::of((0u8..3, 0u64..20).prop_map(|(k, n)| {
                let kind = match k {
                    0 => PreReleaseKind::Alpha,
                    1 => PreReleaseKind::Beta,
                    _ => PreReleaseKind::Rc,
                };
                PreRelease { kind, number: n }
            }));
            let post = prop::option::of(0u64..50);
            let dev = prop::option::of(0u64..50);

            (epoch, release, pre, post, dev).prop_map(|(epoch, release, pre, post, dev)| Version {
                epoch: epoch.unwrap_or(0),
                release,
                pre,
                post,
                dev,
                local: Vec::new(),
            })
        }

        proptest! {
            #[test]
            fn prop_transitivity(
                a in arb_version(),
                b in arb_version(),
                c in arb_version(),
            ) {
                // If a < b and b < c, then a < c
                if a < b && b < c {
                    prop_assert!(a < c, "transitivity violated: {} < {} < {} but not {} < {}", a, b, c, a, c);
                }
            }

            #[test]
            fn prop_antisymmetry(
                a in arb_version(),
                b in arb_version(),
            ) {
                // If a <= b and b <= a, then a == b
                if a <= b && b <= a {
                    prop_assert!(a == b, "antisymmetry violated: {} <= {} and {} <= {} but {} != {}", a, b, b, a, a, b);
                }
            }

            #[test]
            fn prop_parse_round_trip(a in arb_version()) {
                let display = a.to_string();
                let reparsed: Version = display.parse().expect("round-trip parse failed");
                prop_assert!(
                    a == reparsed,
                    "round-trip failed: original={}, displayed='{}', reparsed={}",
                    a, display, reparsed
                );
            }
        }
    }
}
