//! PEP 503 Simple Repository HTML parser.
//!
//! Parses the HTML format returned by PyPI's Simple API, extracting
//! distribution file links with their associated metadata attributes.

use std::collections::HashMap;

use scraper::{Html, Selector};
use url::Url;

use crate::error::Result;
use crate::DistributionFile;

/// Parse a PEP 503 HTML project page into a list of distribution files.
pub fn parse_project_page(html: &str, base_url: &Url) -> Result<Vec<DistributionFile>> {
    let document = Html::parse_document(html);
    let selector = Selector::parse("a").expect("valid CSS selector");

    let mut files = Vec::new();

    for element in document.select(&selector) {
        let href = match element.value().attr("href") {
            Some(h) => h,
            None => continue,
        };

        // Resolve relative URLs against the base
        let resolved = base_url
            .join(href)
            .map(|u| u.to_string())
            .unwrap_or_else(|_| href.to_string());

        // Split URL from hash fragment (e.g., sha256=abcdef)
        let (url, hashes) = split_url_fragment(&resolved);

        // The link text is the filename
        let filename = element.inner_html().trim().to_string();

        // html5ever already decodes HTML entities in attribute values
        let requires_python = element
            .value()
            .attr("data-requires-python")
            .map(|s| s.to_string());

        // PEP 714 renamed data-dist-info-metadata to data-core-metadata.
        // Check the newer name first, fall back to the legacy name.
        let dist_info_metadata = element
            .value()
            .attr("data-core-metadata")
            .or_else(|| element.value().attr("data-dist-info-metadata"))
            .map(|s| s.to_string());

        let yanked = element.value().attr("data-yanked").map(|s| s.to_string());

        files.push(DistributionFile {
            filename,
            url,
            hashes,
            requires_python,
            dist_info_metadata,
            yanked,
        });
    }

    Ok(files)
}

/// Split a URL into the base URL and any hash fragment (algorithm=digest).
fn split_url_fragment(url: &str) -> (String, HashMap<String, String>) {
    let mut hashes = HashMap::new();

    if let Some(hash_idx) = url.find('#') {
        let base_url = url[..hash_idx].to_string();
        let fragment = &url[hash_idx + 1..];

        // Fragment format: algorithm=digest
        if let Some(eq_idx) = fragment.find('=') {
            let algo = &fragment[..eq_idx];
            let digest = &fragment[eq_idx + 1..];
            hashes.insert(algo.to_string(), digest.to_string());
        }

        (base_url, hashes)
    } else {
        (url.to_string(), hashes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_html() {
        let html = r#"
<!DOCTYPE html>
<html>
<head><title>Links for requests</title></head>
<body>
<h1>Links for requests</h1>
<a href="https://files.pythonhosted.org/packages/requests-2.31.0-py3-none-any.whl#sha256=abcdef1234567890" data-requires-python="&gt;=3.7" data-dist-info-metadata="sha256=meta123">requests-2.31.0-py3-none-any.whl</a>
<a href="https://files.pythonhosted.org/packages/requests-2.31.0.tar.gz#sha256=fedcba0987654321" data-requires-python="&gt;=3.7">requests-2.31.0.tar.gz</a>
</body>
</html>"#;

        let base = Url::parse("https://pypi.org/simple/requests/").unwrap();
        let files = parse_project_page(html, &base).unwrap();

        assert_eq!(files.len(), 2);

        assert_eq!(files[0].filename, "requests-2.31.0-py3-none-any.whl");
        assert_eq!(
            files[0].url,
            "https://files.pythonhosted.org/packages/requests-2.31.0-py3-none-any.whl"
        );
        assert_eq!(files[0].hashes.get("sha256").unwrap(), "abcdef1234567890");
        assert_eq!(files[0].requires_python.as_deref(), Some(">=3.7"));
        assert_eq!(
            files[0].dist_info_metadata.as_deref(),
            Some("sha256=meta123")
        );
        assert!(files[0].yanked.is_none());

        assert_eq!(files[1].filename, "requests-2.31.0.tar.gz");
        assert!(files[1].dist_info_metadata.is_none());
    }

    #[test]
    fn test_parse_yanked() {
        let html = r#"
<a href="https://example.com/pkg-1.0.whl" data-yanked="security vulnerability">pkg-1.0.whl</a>
"#;
        let base = Url::parse("https://pypi.org/simple/pkg/").unwrap();
        let files = parse_project_page(html, &base).unwrap();

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].yanked.as_deref(), Some("security vulnerability"));
    }

    #[test]
    fn test_parse_relative_urls() {
        let html = r#"
<a href="../../packages/pkg-1.0.whl#sha256=abc">pkg-1.0.whl</a>
"#;
        let base = Url::parse("https://pypi.org/simple/pkg/").unwrap();
        let files = parse_project_page(html, &base).unwrap();

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].url, "https://pypi.org/packages/pkg-1.0.whl");
    }

    #[test]
    fn test_split_url_fragment() {
        let (url, hashes) = split_url_fragment("https://example.com/pkg.whl#sha256=abcdef");
        assert_eq!(url, "https://example.com/pkg.whl");
        assert_eq!(hashes.get("sha256").unwrap(), "abcdef");
    }

    #[test]
    fn test_split_url_no_fragment() {
        let (url, hashes) = split_url_fragment("https://example.com/pkg.whl");
        assert_eq!(url, "https://example.com/pkg.whl");
        assert!(hashes.is_empty());
    }
}
