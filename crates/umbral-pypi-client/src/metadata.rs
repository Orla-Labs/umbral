use std::collections::HashMap;

use crate::error::{PypiClientError, Result};

/// Parsed Python package metadata (RFC 822 / PEP 566 / PEP 643).
#[derive(Debug, Clone)]
pub struct Metadata {
    pub name: String,
    pub version: String,
    pub requires_dist: Vec<String>,
    pub requires_python: Option<String>,
    pub provides_extra: Vec<String>,
    pub summary: Option<String>,
}

/// Parse an RFC 822-style METADATA file into structured metadata.
///
/// Handles:
/// - `Key: Value` headers
/// - Continuation lines (lines starting with whitespace)
/// - Multi-valued headers (Requires-Dist, Provides-Extra)
pub fn parse_metadata(text: &str) -> Result<Metadata> {
    let mut headers: HashMap<String, Vec<String>> = HashMap::new();
    let mut current_key: Option<String> = None;

    for line in text.lines() {
        // Empty line signals end of headers / start of description body
        if line.is_empty() {
            break;
        }

        // Continuation line: starts with whitespace, appends to current header value
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(ref key) = current_key {
                if let Some(values) = headers.get_mut(key) {
                    if let Some(last) = values.last_mut() {
                        last.push('\n');
                        last.push_str(line.trim());
                    }
                }
            }
            continue;
        }

        // New header line: Key: Value
        if let Some(colon_pos) = line.find(':') {
            let key = line[..colon_pos].to_lowercase();
            let value = line[colon_pos + 1..].trim().to_string();
            current_key = Some(key.clone());
            headers.entry(key).or_default().push(value);
        }
    }

    let name = headers
        .get("name")
        .and_then(|v| v.first())
        .ok_or_else(|| PypiClientError::MetadataParse("missing Name field".into()))?
        .clone();

    let version = headers
        .get("version")
        .and_then(|v| v.first())
        .ok_or_else(|| PypiClientError::MetadataParse("missing Version field".into()))?
        .clone();

    let requires_dist = headers.get("requires-dist").cloned().unwrap_or_default();
    let requires_python = headers
        .get("requires-python")
        .and_then(|v| v.first())
        .cloned();
    let provides_extra = headers.get("provides-extra").cloned().unwrap_or_default();
    let summary = headers.get("summary").and_then(|v| v.first()).cloned();

    Ok(Metadata {
        name,
        version,
        requires_dist,
        requires_python,
        provides_extra,
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_basic_metadata() {
        let text = "\
Metadata-Version: 2.1
Name: requests
Version: 2.31.0
Summary: Python HTTP for Humans.
Requires-Python: >=3.7
Requires-Dist: charset-normalizer<4,>=2
Requires-Dist: idna<4,>=2.5
Requires-Dist: urllib3<3,>=1.21.1
Requires-Dist: certifi>=2017.4.17
Provides-Extra: socks
Provides-Extra: use-chardet-on-py3
Requires-Dist: PySocks!=1.5.7,>=1.5.6; extra == \"socks\"
Requires-Dist: chardet<6,>=3.0.2; extra == \"use-chardet-on-py3\"
";
        let meta = parse_metadata(text).unwrap();
        assert_eq!(meta.name, "requests");
        assert_eq!(meta.version, "2.31.0");
        assert_eq!(meta.summary.as_deref(), Some("Python HTTP for Humans."));
        assert_eq!(meta.requires_python.as_deref(), Some(">=3.7"));
        assert_eq!(meta.requires_dist.len(), 6);
        assert_eq!(meta.provides_extra, vec!["socks", "use-chardet-on-py3"]);
    }

    #[test]
    fn test_parse_continuation_line() {
        let text = "\
Metadata-Version: 2.1
Name: my-package
Version: 1.0.0
Summary: A package with
        a long summary
";
        let meta = parse_metadata(text).unwrap();
        assert_eq!(
            meta.summary.as_deref(),
            Some("A package with\na long summary")
        );
    }

    #[test]
    fn test_missing_name_errors() {
        let text = "\
Metadata-Version: 2.1
Version: 1.0.0
";
        assert!(parse_metadata(text).is_err());
    }

    #[test]
    fn test_missing_version_errors() {
        let text = "\
Metadata-Version: 2.1
Name: my-package
";
        assert!(parse_metadata(text).is_err());
    }

    #[test]
    fn test_body_after_blank_line_ignored() {
        let text = "\
Metadata-Version: 2.1
Name: my-package
Version: 1.0.0

This is the long description body.
It should be ignored by the header parser.
Name: not-a-real-header
";
        let meta = parse_metadata(text).unwrap();
        assert_eq!(meta.name, "my-package");
        assert_eq!(meta.version, "1.0.0");
    }
}
