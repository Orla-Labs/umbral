//! PEP 508 dependency specifier parsing.

use serde::{Deserialize, Serialize};
use std::fmt;
use umbral_pep440::{PackageName, VersionSpecifiers};

use crate::marker::{parse_markers, MarkerTree};

/// A parsed PEP 508 dependency specifier.
///
/// Examples of valid specifiers:
/// - `requests`
/// - `requests>=2.0`
/// - `requests[security]>=2.0`
/// - `requests>=2.0; python_version>="3.6"`
/// - `package @ https://example.com/package.whl`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirement {
    pub name: PackageName,
    pub extras: Vec<String>,
    pub version: Option<VersionSpecifiers>,
    pub url: Option<String>,
    pub marker: Option<MarkerTree>,
}

impl Requirement {
    /// Parse a PEP 508 dependency specifier string.
    pub fn parse(input: &str) -> Result<Self, String> {
        let mut parser = Parser::new(input);
        parser.parse_requirement()
    }
}

impl fmt::Display for Requirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name.source_name())?;
        if !self.extras.is_empty() {
            write!(f, "[{}]", self.extras.join(", "))?;
        }
        if let Some(ref url) = self.url {
            write!(f, " @ {url}")?;
        } else if let Some(ref version) = self.version {
            if !version.is_empty() {
                write!(f, " {version}")?;
            }
        }
        if let Some(ref marker) = self.marker {
            write!(f, "; {marker}")?;
        }
        Ok(())
    }
}

// ── Recursive-descent parser ────────────────────────────────────────────────

struct Parser<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    fn remaining(&self) -> &'a str {
        &self.input[self.pos..]
    }

    fn skip_ws(&mut self) {
        while self.pos < self.input.len() && self.input.as_bytes()[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    /// Top-level: parse a full PEP 508 requirement string.
    fn parse_requirement(&mut self) -> Result<Requirement, String> {
        self.skip_ws();
        let name = self.parse_name()?;
        self.skip_ws();
        let extras = self.parse_extras()?;
        self.skip_ws();

        let (version, url) = if self.peek() == Some('@') {
            self.pos += 1;
            self.skip_ws();
            let url = self.parse_url()?;
            (None, Some(url))
        } else {
            let version = self.parse_version_specifiers()?;
            (version, None)
        };

        self.skip_ws();
        let marker = if self.peek() == Some(';') {
            self.pos += 1;
            self.skip_ws();
            let marker_str = self.remaining().trim();
            if marker_str.is_empty() {
                return Err("empty marker expression after ';'".into());
            }
            let marker = parse_markers(marker_str)?;
            self.pos = self.input.len();
            Some(marker)
        } else {
            None
        };

        self.skip_ws();
        if self.pos < self.input.len() {
            return Err(format!(
                "unexpected trailing content: '{}'",
                self.remaining()
            ));
        }

        Ok(Requirement {
            name: PackageName::new(name),
            extras,
            version,
            url,
            marker,
        })
    }

    /// Parse a PEP 508 identifier (package name or extra name).
    fn parse_name(&mut self) -> Result<String, String> {
        let start = self.pos;
        while self.pos < self.input.len() {
            let c = self.input.as_bytes()[self.pos];
            if c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c == b'.' {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.pos == start {
            return Err("expected package name".into());
        }
        Ok(self.input[start..self.pos].to_string())
    }

    /// Parse optional extras: `[extra1, extra2]`.
    fn parse_extras(&mut self) -> Result<Vec<String>, String> {
        if self.peek() != Some('[') {
            return Ok(Vec::new());
        }
        self.pos += 1; // consume '['
        let mut extras = Vec::new();

        loop {
            self.skip_ws();
            if self.peek() == Some(']') {
                self.pos += 1;
                return Ok(extras);
            }
            if !extras.is_empty() {
                if self.peek() != Some(',') {
                    return Err("expected ',' or ']' in extras".into());
                }
                self.pos += 1; // consume ','
                self.skip_ws();
            }
            let extra = self.parse_name()?;
            extras.push(extra.to_lowercase());
        }
    }

    /// Parse the URL after `@`, up to `;` (marker separator) or end-of-input.
    fn parse_url(&mut self) -> Result<String, String> {
        let rest = self.remaining();
        let url_end = rest.find(';').unwrap_or(rest.len());
        let url = rest[..url_end].trim().to_string();
        self.pos += url_end;
        if url.is_empty() {
            return Err("expected URL after '@'".into());
        }
        Ok(url)
    }

    /// Parse optional version specifiers, with or without parentheses.
    fn parse_version_specifiers(&mut self) -> Result<Option<VersionSpecifiers>, String> {
        self.skip_ws();
        let rest = self.remaining();

        if rest.is_empty() || rest.starts_with(';') {
            return Ok(None);
        }

        // Handle parenthesized version specifiers: name (>=1.0, <2.0)
        let (spec_str, advance) = if rest.starts_with('(') {
            let close = rest
                .find(')')
                .ok_or("unclosed parenthesis in version specifier")?;
            (rest[1..close].trim(), close + 1)
        } else {
            let end = rest.find(';').unwrap_or(rest.len());
            (rest[..end].trim(), end)
        };

        self.pos += advance;

        if spec_str.is_empty() {
            return Ok(None);
        }

        let specs: VersionSpecifiers = spec_str
            .parse()
            .map_err(|e: umbral_pep440::ParseError| format!("invalid version specifier: {e}"))?;

        if specs.is_empty() {
            Ok(None)
        } else {
            Ok(Some(specs))
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Basic parsing ───────────────────────────────────────────────────

    #[test]
    fn parse_name_only() {
        let req = Requirement::parse("requests").unwrap();
        assert_eq!(req.name.as_str(), "requests");
        assert!(req.extras.is_empty());
        assert!(req.version.is_none());
        assert!(req.url.is_none());
        assert!(req.marker.is_none());
    }

    #[test]
    fn parse_with_version() {
        let req = Requirement::parse("requests>=2.0").unwrap();
        assert_eq!(req.name.as_str(), "requests");
        assert!(req.version.is_some());
        assert_eq!(req.version.unwrap().0.len(), 1);
    }

    #[test]
    fn parse_with_multiple_versions() {
        let req = Requirement::parse("requests>=2.0,<3.0").unwrap();
        assert_eq!(req.name.as_str(), "requests");
        assert_eq!(req.version.unwrap().0.len(), 2);
    }

    #[test]
    fn parse_with_extras() {
        let req = Requirement::parse("requests[security]>=2.0").unwrap();
        assert_eq!(req.name.as_str(), "requests");
        assert_eq!(req.extras, vec!["security"]);
        assert!(req.version.is_some());
    }

    #[test]
    fn parse_with_multiple_extras() {
        let req = Requirement::parse("package[extra1, extra2]>=1.0").unwrap();
        assert_eq!(req.extras, vec!["extra1", "extra2"]);
    }

    #[test]
    fn parse_empty_extras() {
        let req = Requirement::parse("requests[]>=2.0").unwrap();
        assert!(req.extras.is_empty());
        assert!(req.version.is_some());
    }

    #[test]
    fn parse_with_markers() {
        let req = Requirement::parse("requests>=2.0; python_version>=\"3.6\"").unwrap();
        assert_eq!(req.name.as_str(), "requests");
        assert!(req.version.is_some());
        assert!(req.marker.is_some());
    }

    #[test]
    fn parse_name_with_markers_no_version() {
        let req = Requirement::parse("requests; python_version >= \"3.8\"").unwrap();
        assert_eq!(req.name.as_str(), "requests");
        assert!(req.version.is_none());
        assert!(req.marker.is_some());
    }

    #[test]
    fn parse_url_dependency() {
        let req = Requirement::parse("package @ https://example.com/package.whl").unwrap();
        assert_eq!(req.name.as_str(), "package");
        assert_eq!(req.url.as_deref(), Some("https://example.com/package.whl"));
        assert!(req.version.is_none());
    }

    #[test]
    fn parse_url_with_markers() {
        let req = Requirement::parse(
            "package @ https://example.com/package.whl ; python_version >= \"3.8\"",
        )
        .unwrap();
        assert_eq!(req.name.as_str(), "package");
        assert!(req.url.is_some());
        assert!(req.marker.is_some());
    }

    #[test]
    fn parse_complex_markers() {
        let req =
            Requirement::parse("package>=1.0; python_version >= \"3.8\" and os_name == \"posix\"")
                .unwrap();
        assert!(req.marker.is_some());
        match req.marker.unwrap() {
            MarkerTree::And(children) => assert_eq!(children.len(), 2),
            other => panic!("expected And, got: {other:?}"),
        }
    }

    #[test]
    fn parse_with_spaces() {
        let req = Requirement::parse("  requests  >= 2.0  ").unwrap();
        assert_eq!(req.name.as_str(), "requests");
        assert!(req.version.is_some());
    }

    #[test]
    fn parse_version_with_parens() {
        let req = Requirement::parse("name (>=1.0, <2.0)").unwrap();
        assert_eq!(req.name.as_str(), "name");
        assert_eq!(req.version.unwrap().0.len(), 2);
    }

    #[test]
    fn parse_version_with_parens_and_markers() {
        let req = Requirement::parse("name (>=1.0, <2.0); python_version >= \"3.8\"").unwrap();
        assert_eq!(req.name.as_str(), "name");
        assert!(req.version.is_some());
        assert!(req.marker.is_some());
    }

    // ── PEP 508 standard examples ───────────────────────────────────────

    #[test]
    fn parse_pep508_examples() {
        let examples = &[
            "A",
            "A.B-C_D",
            "aa",
            "name",
            "name<=1",
            "name>=3",
            "name>=3,<2",
            "name[quux, strange]",
            "name[quux, strange]>=1.0",
            "name @ https://example.com/name.tar.gz",
        ];
        for ex in examples {
            assert!(Requirement::parse(ex).is_ok(), "failed to parse: {ex}");
        }
    }

    // ── Name normalization ──────────────────────────────────────────────

    #[test]
    fn name_normalization() {
        let req = Requirement::parse("Requests>=2.0").unwrap();
        assert_eq!(req.name.as_str(), "requests");
        // Display preserves original source name.
        assert!(req.to_string().starts_with("Requests"));
    }

    #[test]
    fn name_with_hyphens_underscores() {
        let r1 = Requirement::parse("my-package>=1.0").unwrap();
        let r2 = Requirement::parse("my_package>=1.0").unwrap();
        assert_eq!(r1.name, r2.name);
    }

    // ── Display ─────────────────────────────────────────────────────────

    #[test]
    fn display_name_only() {
        let req = Requirement::parse("requests").unwrap();
        assert_eq!(req.to_string(), "requests");
    }

    #[test]
    fn display_with_version() {
        let req = Requirement::parse("requests>=2.0").unwrap();
        assert_eq!(req.to_string(), "requests >=2.0");
    }

    #[test]
    fn display_with_extras() {
        let req = Requirement::parse("requests[security]>=2.0").unwrap();
        assert_eq!(req.to_string(), "requests[security] >=2.0");
    }

    #[test]
    fn display_with_multiple_extras() {
        let req = Requirement::parse("package[e1, e2]>=1.0").unwrap();
        assert_eq!(req.to_string(), "package[e1, e2] >=1.0");
    }

    #[test]
    fn display_url() {
        let req = Requirement::parse("package @ https://example.com/package.whl").unwrap();
        assert_eq!(req.to_string(), "package @ https://example.com/package.whl");
    }

    #[test]
    fn display_with_markers() {
        let req = Requirement::parse("requests>=2.0; python_version >= \"3.8\"").unwrap();
        assert_eq!(req.to_string(), "requests >=2.0; python_version >= \"3.8\"");
    }

    // ── Round-trip ──────────────────────────────────────────────────────

    #[test]
    fn roundtrip_complex() {
        let input = "requests[security]>=2.0; python_version >= \"3.8\" and os_name == \"posix\"";
        let req = Requirement::parse(input).unwrap();
        let displayed = req.to_string();
        // Re-parse the displayed version.
        let req2 = Requirement::parse(&displayed).unwrap();
        assert_eq!(req.name, req2.name);
        assert_eq!(req.extras, req2.extras);
        assert_eq!(req.marker, req2.marker);
    }

    #[test]
    fn roundtrip_url_with_markers() {
        let req =
            Requirement::parse("pkg @ https://example.com/pkg.whl ; os_name == \"posix\"").unwrap();
        let displayed = req.to_string();
        let req2 = Requirement::parse(&displayed).unwrap();
        assert_eq!(req.name, req2.name);
        assert_eq!(req.url, req2.url);
        assert_eq!(req.marker, req2.marker);
    }

    // ── Error cases ─────────────────────────────────────────────────────

    #[test]
    fn parse_error_empty() {
        assert!(Requirement::parse("").is_err());
    }

    #[test]
    fn parse_error_only_spaces() {
        assert!(Requirement::parse("   ").is_err());
    }

    #[test]
    fn parse_error_empty_marker() {
        assert!(Requirement::parse("requests;").is_err());
    }

    #[test]
    fn parse_error_bad_version() {
        assert!(Requirement::parse("requests>=").is_err());
    }

    // ── Extra normalization ───────────────────────────────────────────

    #[test]
    fn extras_normalized_to_lowercase() {
        let req = Requirement::parse("package[Security]>=1.0").unwrap();
        assert_eq!(
            req.extras,
            vec!["security"],
            "extras should be lowercased at parse time"
        );
    }

    #[test]
    fn multiple_extras_normalized_to_lowercase() {
        let req = Requirement::parse("package[Security, DEV]>=1.0").unwrap();
        assert_eq!(
            req.extras,
            vec!["security", "dev"],
            "all extras should be lowercased at parse time"
        );
    }

    // ── Edge case: URL requirement with hash fragment ───────────

    #[test]
    fn parse_url_with_hash_fragment() {
        let req = Requirement::parse(
            "package @ https://example.com/package-1.0.whl#sha256=abcdef1234567890",
        )
        .unwrap();
        assert_eq!(req.name.as_str(), "package");
        assert_eq!(
            req.url.as_deref(),
            Some("https://example.com/package-1.0.whl#sha256=abcdef1234567890")
        );
        assert!(req.version.is_none());
        // Round-trip preserves the URL including hash fragment
        let displayed = req.to_string();
        let reparsed = Requirement::parse(&displayed).unwrap();
        assert_eq!(req.url, reparsed.url);
    }

    // ── Edge case: URL requirement with extras ─────────────────

    #[test]
    fn parse_url_with_extras() {
        let req =
            Requirement::parse("package[security] @ https://example.com/package-1.0.whl").unwrap();
        assert_eq!(req.name.as_str(), "package");
        assert_eq!(req.extras, vec!["security"]);
        assert_eq!(
            req.url.as_deref(),
            Some("https://example.com/package-1.0.whl")
        );
        assert!(req.version.is_none());
    }

    // ── Edge case: URL with extras and markers ─────────────────

    #[test]
    fn parse_url_with_extras_and_markers() {
        let req = Requirement::parse(
            "package[security, socks] @ https://example.com/pkg.whl ; python_version >= \"3.8\"",
        )
        .unwrap();
        assert_eq!(req.name.as_str(), "package");
        assert_eq!(req.extras, vec!["security", "socks"]);
        assert!(req.url.is_some());
        assert!(req.marker.is_some());
    }

    // ── Edge case: URL with hash fragment, extras, and markers ─

    #[test]
    fn parse_url_with_hash_extras_markers() {
        let req = Requirement::parse(
            "package[extra1] @ https://example.com/p.whl#md5=abc ; os_name == \"posix\"",
        )
        .unwrap();
        assert_eq!(req.extras, vec!["extra1"]);
        assert!(
            req.url.as_deref().unwrap().contains("#md5=abc"),
            "URL should preserve hash fragment"
        );
        assert!(req.marker.is_some());
    }
}
