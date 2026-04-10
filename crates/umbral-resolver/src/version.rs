//! Wrapper around PEP 440 `Version` that implements pubgrub's `Version` trait.
//!
//! The key challenge: pubgrub requires `lowest()` and `bump()` methods on versions.
//! PEP 440 versions have complex ordering (epoch, release, pre/post/dev, local).
//! We solve this with a `bump_count` field that creates virtual successor versions
//! for pubgrub's half-open interval ranges, without needing to enumerate all
//! possible PEP 440 versions between two points.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

use umbral_pep440::Version;

/// Wrapper around PEP 440 `Version` that implements pubgrub's `Version` trait.
///
/// Real package versions always have `bump_count = 0`. The `bump_count` field
/// exists solely to enable pubgrub's range operations: `Range::exact(v)` creates
/// `[v(0), v(1))` which contains only the exact version.
#[derive(Debug, Clone)]
pub struct UmbralVersion {
    pub(crate) inner: Version,
    pub(crate) bump_count: u32,
}

impl UmbralVersion {
    pub fn new(version: Version) -> Self {
        Self {
            inner: version,
            bump_count: 0,
        }
    }

    pub fn inner(&self) -> &Version {
        &self.inner
    }

    pub fn is_prerelease(&self) -> bool {
        self.inner.is_prerelease()
    }
}

impl From<Version> for UmbralVersion {
    fn from(version: Version) -> Self {
        Self::new(version)
    }
}

impl PartialEq for UmbralVersion {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for UmbralVersion {}

impl PartialOrd for UmbralVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for UmbralVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.inner
            .cmp(&other.inner)
            .then(self.bump_count.cmp(&other.bump_count))
    }
}

impl Hash for UmbralVersion {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.inner.epoch.hash(state);
        // Trim trailing zeros for consistent hashing: 1.0 and 1.0.0 must hash equal
        // because they compare equal via PEP 440 Ord (which zero-pads).
        let release = &self.inner.release;
        let significant_len = release.iter().rposition(|&x| x != 0).map_or(1, |i| i + 1);
        release[..significant_len].hash(state);
        self.inner.pre.hash(state);
        self.inner.post.hash(state);
        self.inner.dev.hash(state);
        self.inner.local.hash(state);
        self.bump_count.hash(state);
    }
}

impl fmt::Display for UmbralVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.inner)
    }
}

impl pubgrub::version::Version for UmbralVersion {
    fn lowest() -> Self {
        // The lowest possible PEP 440 version: 0.dev0
        let mut v = Version::new(vec![0]);
        v.dev = Some(0);
        Self {
            inner: v,
            bump_count: 0,
        }
    }

    fn bump(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            bump_count: self.bump_count.saturating_add(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pubgrub::version::Version as PubgrubVersion;

    #[test]
    fn test_lowest_is_lowest() {
        let lowest = UmbralVersion::lowest();
        assert_eq!(lowest.inner.to_string(), "0.dev0");
    }

    #[test]
    fn test_bump_is_strictly_greater() {
        let v: UmbralVersion = "1.0.0".parse::<Version>().unwrap().into();
        let bumped = v.bump();
        assert!(bumped > v);
    }

    #[test]
    fn test_real_version_between_original_and_bumped() {
        // A real version greater than v should also be greater than v.bump()
        let v: UmbralVersion = "1.0.0".parse::<Version>().unwrap().into();
        let v_bumped = v.bump();
        let v_next: UmbralVersion = "1.0.1".parse::<Version>().unwrap().into();
        assert!(v_next > v_bumped, "1.0.1 should be > 1.0.0.bump()");
    }

    #[test]
    fn test_hash_consistency() {
        use std::collections::hash_map::DefaultHasher;

        let v1: UmbralVersion = "1.0".parse::<Version>().unwrap().into();
        let v2: UmbralVersion = "1.0.0".parse::<Version>().unwrap().into();
        assert_eq!(v1, v2, "1.0 and 1.0.0 should be equal");

        let hash = |v: &UmbralVersion| {
            let mut h = DefaultHasher::new();
            v.hash(&mut h);
            h.finish()
        };
        assert_eq!(
            hash(&v1),
            hash(&v2),
            "equal versions must have equal hashes"
        );
    }

    #[test]
    fn test_prerelease_ordering() {
        let dev: UmbralVersion = "1.0.dev0".parse::<Version>().unwrap().into();
        let alpha: UmbralVersion = "1.0a1".parse::<Version>().unwrap().into();
        let final_v: UmbralVersion = "1.0.0".parse::<Version>().unwrap().into();
        let post: UmbralVersion = "1.0.0.post1".parse::<Version>().unwrap().into();

        assert!(dev < alpha);
        assert!(alpha < final_v);
        assert!(final_v < post);
    }

    #[test]
    fn test_lowest_below_all_real_versions() {
        let lowest = UmbralVersion::lowest();
        let zero: UmbralVersion = "0".parse::<Version>().unwrap().into();
        let one: UmbralVersion = "1.0".parse::<Version>().unwrap().into();
        assert!(lowest < zero);
        assert!(lowest < one);
    }
}
