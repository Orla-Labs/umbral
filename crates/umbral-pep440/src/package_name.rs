//! PEP 503 package name normalization.
//!
//! Package names are normalized by lowercasing and replacing any run of
//! underscores, hyphens, or periods with a single hyphen.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::LazyLock;

static NORMALIZE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[-_.]+").expect("NORMALIZE_RE is a valid regex"));

/// A PEP 503-normalized package name.
///
/// Internally stores the normalized form. Display shows the normalized form.
/// Equality and hashing use the normalized form.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageName {
    /// The original name as provided.
    source: String,
    /// The PEP 503-normalized name (lowercase, hyphens for separators).
    normalized: String,
}

impl PackageName {
    /// Create a new PackageName, normalizing per PEP 503.
    pub fn new(name: impl Into<String>) -> Self {
        let source = name.into();
        let normalized = normalize(&source);
        Self { source, normalized }
    }

    /// The normalized name.
    pub fn as_str(&self) -> &str {
        &self.normalized
    }

    /// The original source name.
    pub fn source_name(&self) -> &str {
        &self.source
    }
}

/// Normalize a package name per PEP 503.
fn normalize(name: &str) -> String {
    NORMALIZE_RE
        .replace_all(&name.to_lowercase(), "-")
        .into_owned()
}

impl fmt::Display for PackageName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.normalized)
    }
}

impl PartialEq for PackageName {
    fn eq(&self, other: &Self) -> bool {
        self.normalized == other.normalized
    }
}

impl Eq for PackageName {}

impl std::hash::Hash for PackageName {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.normalized.hash(state);
    }
}

impl PartialOrd for PackageName {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PackageName {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.normalized.cmp(&other.normalized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalization() {
        // All of these should normalize to the same thing
        let names = vec![
            "Friendly-Bard",
            "Friendly.Bard",
            "Friendly_Bard",
            "friendly-bard",
            "friendly.bard",
            "friendly_bard",
            "FRIENDLY-BARD",
            "Friendly--Bard",
            "Friendly.__.-Bard",
        ];

        for name in &names {
            let pn = PackageName::new(*name);
            assert_eq!(pn.as_str(), "friendly-bard", "failed for input: {name}");
        }
    }

    #[test]
    fn test_equality() {
        let a = PackageName::new("Requests");
        let b = PackageName::new("requests");
        let c = PackageName::new("REQUESTS");
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn test_display() {
        let pn = PackageName::new("My_Package.Name");
        assert_eq!(pn.to_string(), "my-package-name");
    }

    #[test]
    fn test_source_preserved() {
        let pn = PackageName::new("My_Package");
        assert_eq!(pn.source_name(), "My_Package");
        assert_eq!(pn.as_str(), "my-package");
    }
}
