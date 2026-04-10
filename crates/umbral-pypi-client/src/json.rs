//! PEP 691 Simple Repository JSON parser.
//!
//! Parses the JSON format for PyPI's Simple API, which is the preferred
//! format when content negotiation succeeds.

use std::collections::HashMap;

use serde::Deserialize;

use crate::error::Result;
use crate::DistributionFile;

/// Top-level PEP 691 JSON response for a project page.
#[derive(Debug, Deserialize)]
pub struct JsonProjectPage {
    #[allow(dead_code)]
    pub name: String,
    pub files: Vec<JsonDistFile>,
}

/// A single file entry in the PEP 691 JSON response.
#[derive(Debug, Deserialize)]
pub struct JsonDistFile {
    pub filename: String,
    pub url: String,
    #[serde(default)]
    pub hashes: HashMap<String, String>,
    #[serde(rename = "requires-python")]
    pub requires_python: Option<String>,
    /// PEP 714 `core-metadata` (preferred over `dist-info-metadata`).
    #[serde(rename = "core-metadata")]
    pub core_metadata: Option<serde_json::Value>,
    /// Legacy PEP 658 `dist-info-metadata`.
    #[serde(rename = "dist-info-metadata")]
    pub dist_info_metadata: Option<serde_json::Value>,
    pub yanked: Option<serde_json::Value>,
}

/// Parse a PEP 691 JSON project page into a list of distribution files.
pub fn parse_project_page(json: &str) -> Result<Vec<DistributionFile>> {
    let page: JsonProjectPage = serde_json::from_str(json)?;

    let files = page
        .files
        .into_iter()
        .map(|f| {
            // PEP 714: prefer core-metadata over dist-info-metadata
            let metadata_value = f.core_metadata.or(f.dist_info_metadata);
            let dist_info_metadata = metadata_value.and_then(|v| match v {
                serde_json::Value::Bool(true) => Some("true".to_string()),
                serde_json::Value::Bool(false) => None,
                serde_json::Value::String(s) => Some(s),
                serde_json::Value::Object(map) => {
                    // PEP 714: { "sha256": "..." }
                    map.iter()
                        .next()
                        .map(|(k, v)| format!("{}={}", k, v.as_str().unwrap_or_default()))
                }
                _ => None,
            });

            let yanked = f.yanked.and_then(|v| match v {
                serde_json::Value::Bool(false) => None,
                serde_json::Value::Bool(true) => Some(String::new()),
                serde_json::Value::String(s) => Some(s),
                _ => None,
            });

            DistributionFile {
                filename: f.filename,
                url: f.url,
                hashes: f.hashes,
                requires_python: f.requires_python,
                dist_info_metadata,
                yanked,
            }
        })
        .collect();

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_json_project_page() {
        let json = r#"{
  "name": "requests",
  "files": [
    {
      "filename": "requests-2.31.0-py3-none-any.whl",
      "url": "https://files.pythonhosted.org/packages/requests-2.31.0-py3-none-any.whl",
      "hashes": {"sha256": "abcdef1234567890"},
      "requires-python": ">=3.7",
      "dist-info-metadata": {"sha256": "meta123"}
    },
    {
      "filename": "requests-2.31.0.tar.gz",
      "url": "https://files.pythonhosted.org/packages/requests-2.31.0.tar.gz",
      "hashes": {"sha256": "fedcba0987654321"},
      "requires-python": ">=3.7",
      "yanked": false
    }
  ]
}"#;

        let files = parse_project_page(json).unwrap();
        assert_eq!(files.len(), 2);

        assert_eq!(files[0].filename, "requests-2.31.0-py3-none-any.whl");
        assert_eq!(files[0].hashes.get("sha256").unwrap(), "abcdef1234567890");
        assert_eq!(files[0].requires_python.as_deref(), Some(">=3.7"));
        assert_eq!(
            files[0].dist_info_metadata.as_deref(),
            Some("sha256=meta123")
        );
        assert!(files[0].yanked.is_none());

        assert!(files[1].yanked.is_none()); // false → None
    }

    #[test]
    fn test_parse_yanked_with_reason() {
        let json = r#"{
  "name": "pkg",
  "files": [
    {
      "filename": "pkg-1.0.whl",
      "url": "https://example.com/pkg-1.0.whl",
      "hashes": {},
      "yanked": "security vulnerability"
    }
  ]
}"#;
        let files = parse_project_page(json).unwrap();
        assert_eq!(files[0].yanked.as_deref(), Some("security vulnerability"));
    }

    #[test]
    fn test_parse_bool_dist_info_metadata() {
        let json = r#"{
  "name": "pkg",
  "files": [
    {
      "filename": "pkg-1.0.whl",
      "url": "https://example.com/pkg-1.0.whl",
      "hashes": {},
      "dist-info-metadata": true
    }
  ]
}"#;
        let files = parse_project_page(json).unwrap();
        assert_eq!(files[0].dist_info_metadata.as_deref(), Some("true"));
    }
}
