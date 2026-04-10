//! Async PyPI Simple API client with content negotiation, caching, and retries.

use std::path::PathBuf;
use std::time::Duration;

use reqwest::{header, Client, StatusCode};
use tracing::{debug, warn};
use url::Url;

use umbral_pep440::PackageName;

use crate::cache::{CacheEntry, DiskCache};
use crate::error::{PypiClientError, Result};
use crate::{DistributionFile, Metadata, ProjectPage};

const RETRY_DELAYS: &[Duration] = &[
    Duration::from_millis(200),
    Duration::from_secs(1),
    Duration::from_secs(5),
];

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 120;

fn request_timeout() -> Duration {
    let secs = std::env::var("UMBRAL_HTTP_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_REQUEST_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// An async client for PyPI's Simple Repository API.
///
/// Supports both PEP 691 (JSON) and PEP 503 (HTML) responses via content
/// negotiation, with disk caching and automatic retries.
///
/// When `extra_urls` are configured, the client will try the primary index
/// first. If the package is not found (HTTP error or empty file list), each
/// extra index URL is tried in order until a non-empty result is obtained.
pub struct SimpleApiClient {
    client: Client,
    index_url: Url,
    extra_urls: Vec<Url>,
    cache: DiskCache,
}

impl SimpleApiClient {
    /// Create a new client targeting the given index URL.
    ///
    /// `index_url` should be the base URL for the Simple API (e.g.,
    /// `https://pypi.org/simple/`). `cache_dir` is the directory for
    /// disk-cached responses.
    pub fn new(index_url: Url, cache_dir: PathBuf) -> Result<Self> {
        Self::with_extra_urls(index_url, vec![], cache_dir)
    }

    /// Create a new client with additional fallback index URLs.
    ///
    /// When fetching a project page, the primary `index_url` is tried first.
    /// If the package is not found or has no files, each URL in `extra_urls`
    /// is tried in order.
    pub fn with_extra_urls(
        index_url: Url,
        extra_urls: Vec<Url>,
        cache_dir: PathBuf,
    ) -> Result<Self> {
        let client = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(request_timeout())
            .user_agent(format!("umbral/{}", env!("CARGO_PKG_VERSION")))
            .build()?;

        Ok(Self {
            client,
            index_url,
            extra_urls,
            cache: DiskCache::new(cache_dir),
        })
    }

    /// Create a client with a pre-built reqwest `Client` (for testing).
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn with_client(client: Client, index_url: Url, cache_dir: PathBuf) -> Self {
        Self {
            client,
            index_url,
            extra_urls: vec![],
            cache: DiskCache::new(cache_dir),
        }
    }

    /// Create a client with a pre-built reqwest `Client` and extra URLs (for testing).
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn with_client_and_extras(
        client: Client,
        index_url: Url,
        extra_urls: Vec<Url>,
        cache_dir: PathBuf,
    ) -> Self {
        Self {
            client,
            index_url,
            extra_urls,
            cache: DiskCache::new(cache_dir),
        }
    }

    /// Return a reference to the underlying disk cache.
    pub fn cache(&self) -> &DiskCache {
        &self.cache
    }

    /// Fetch and parse a project page from the Simple API.
    ///
    /// The project name is normalized per PEP 503. Content negotiation
    /// prefers JSON (PEP 691) but falls back to HTML (PEP 503).
    ///
    /// When extra index URLs are configured, the primary index is tried first.
    /// If the result is an error or has an empty file list, each extra index
    /// is tried in order. The first non-empty result is returned; if all
    /// indexes fail, the primary result is returned.
    pub async fn fetch_project_page(&self, name: &str) -> Result<ProjectPage> {
        let primary_result = self.fetch_project_page_from(&self.index_url, name).await;

        // If we have no extra URLs, return immediately.
        if self.extra_urls.is_empty() {
            return primary_result;
        }

        // If primary succeeded with files, return immediately.
        match &primary_result {
            Ok(page) if !page.files.is_empty() => return primary_result,
            _ => {}
        }

        // Try each extra index in order.
        for extra_url in &self.extra_urls {
            debug!(index = %extra_url, package = name, "trying extra index");
            match self.fetch_project_page_from(extra_url, name).await {
                Ok(page) if !page.files.is_empty() => return Ok(page),
                _ => continue,
            }
        }

        // All extras failed or were empty; return the primary result.
        primary_result
    }

    /// Fetch and parse a project page from a specific index URL.
    async fn fetch_project_page_from(&self, base_url: &Url, name: &str) -> Result<ProjectPage> {
        let normalized = PackageName::new(name).to_string();
        let url = base_url.join(&format!("{normalized}/"))?;
        let cache_key = format!(
            "projects/{}/{normalized}",
            base_url.host_str().unwrap_or("unknown")
        );

        let cached = self.cache.read(&cache_key);
        let (body, content_type) = self.fetch_with_retry(&url, &cache_key, &cached).await?;

        // Use Content-Type to decide parsing strategy. Fall back to
        // trial-parse if the header is missing or unrecognized.
        let ct = content_type.as_deref().unwrap_or("");
        let files = if ct.contains("application/vnd.pypi.simple") && ct.contains("json") {
            crate::json::parse_project_page(&body)?
        } else if ct.contains("text/html") {
            crate::html::parse_project_page(&body, &url)?
        } else {
            // Unrecognized or missing Content-Type — trial parse
            match serde_json::from_str::<crate::json::JsonProjectPage>(&body) {
                Ok(_) => crate::json::parse_project_page(&body)?,
                Err(_) => crate::html::parse_project_page(&body, &url)?,
            }
        };

        Ok(ProjectPage { files })
    }

    /// Fetch package metadata for a distribution file (PEP 658).
    ///
    /// Downloads `{url}.metadata` and parses the RFC 822-format METADATA file.
    pub async fn fetch_metadata(&self, file: &DistributionFile) -> Result<Metadata> {
        let metadata_url = format!("{}.metadata", file.url);
        let url: Url = metadata_url.parse()?;
        let cache_key = format!("metadata/{}", file.filename);

        let cached = self.cache.read(&cache_key);
        let (body, _content_type) = self.fetch_with_retry(&url, &cache_key, &cached).await?;

        crate::metadata::parse_metadata(&body)
    }

    /// Perform an HTTP GET with up to 3 retries and exponential backoff.
    ///
    /// Returns `(body, content_type)`.
    async fn fetch_with_retry(
        &self,
        url: &Url,
        cache_key: &str,
        cached: &Option<(String, CacheEntry)>,
    ) -> Result<(String, Option<String>)> {
        let mut last_err = None;

        for (attempt, delay) in std::iter::once(&Duration::ZERO)
            .chain(RETRY_DELAYS.iter())
            .enumerate()
        {
            if attempt > 0 {
                debug!(attempt, ?delay, %url, "retrying request");
                tokio::time::sleep(*delay).await;
            }

            match self.do_fetch(url, cache_key, cached).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    warn!(attempt, %url, error = %e, "request failed");
                    last_err = Some(e);
                }
            }
        }

        Err(PypiClientError::RetryExhausted {
            attempts: (RETRY_DELAYS.len() + 1) as u32,
            message: last_err.map(|e| e.to_string()).unwrap_or_default(),
        })
    }

    /// Execute a single HTTP GET request with caching support.
    ///
    /// Returns `(body, content_type)`.
    async fn do_fetch(
        &self,
        url: &Url,
        cache_key: &str,
        cached: &Option<(String, CacheEntry)>,
    ) -> Result<(String, Option<String>)> {
        let mut req = self.client.get(url.clone()).header(
            header::ACCEPT,
            "application/vnd.pypi.simple.v1+json, text/html;q=0.1",
        );

        // Add conditional request headers from cache
        if let Some((_, ref entry)) = cached {
            if let Some(ref etag) = entry.etag {
                req = req.header(header::IF_NONE_MATCH, etag);
            }
            if let Some(ref last_modified) = entry.last_modified {
                req = req.header(header::IF_MODIFIED_SINCE, last_modified);
            }
        }

        let response = req.send().await?;
        let status = response.status();

        // 304 Not Modified — return cached content
        if status == StatusCode::NOT_MODIFIED {
            if let Some((ref content, _)) = cached {
                debug!(%url, "304 Not Modified, using cache");
                return Ok((content.clone(), None));
            }
        }

        // Check for HTTP errors after handling 304
        let response = response.error_for_status()?;

        // Extract content type before consuming the response
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        // Extract cache headers
        let etag = response
            .headers()
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let last_modified = response
            .headers()
            .get(header::LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let body = response.text().await?;

        // Cache the response
        let entry = CacheEntry {
            etag,
            last_modified,
        };
        if let Err(e) = self.cache.write(cache_key, &body, &entry) {
            warn!(error = %e, "failed to write cache");
        }

        Ok((body, content_type))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Wiremock integration tests ───────────────────────────────────

    mod wiremock_tests {
        use super::*;
        use std::fs;
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        /// Helper: create a `SimpleApiClient` pointed at the given mock server.
        fn make_client(server: &MockServer, cache_dir: &std::path::Path) -> SimpleApiClient {
            let index_url = Url::parse(&format!("{}/simple/", server.uri())).unwrap();
            SimpleApiClient::with_client(
                Client::builder()
                    .timeout(Duration::from_secs(5))
                    .build()
                    .unwrap(),
                index_url,
                cache_dir.to_path_buf(),
            )
        }

        #[tokio::test]
        async fn test_json_response_parsing() {
            let server = MockServer::start().await;
            let cache_dir = std::env::temp_dir().join("umbral-wiremock-json");
            let _ = fs::remove_dir_all(&cache_dir);

            let json_body = r#"{
                "name": "requests",
                "files": [
                    {
                        "filename": "requests-2.31.0-py3-none-any.whl",
                        "url": "https://files.example.com/requests-2.31.0-py3-none-any.whl",
                        "hashes": {"sha256": "abc123"},
                        "requires-python": ">=3.7",
                        "dist-info-metadata": true
                    }
                ]
            }"#;

            Mock::given(method("GET"))
                .and(path("/simple/requests/"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_string(json_body)
                        .insert_header("content-type", "application/vnd.pypi.simple.v1+json"),
                )
                .expect(1)
                .mount(&server)
                .await;

            let client = make_client(&server, &cache_dir);
            let page = client.fetch_project_page("requests").await.unwrap();

            assert_eq!(page.files.len(), 1);
            assert_eq!(page.files[0].filename, "requests-2.31.0-py3-none-any.whl");
            assert_eq!(page.files[0].requires_python.as_deref(), Some(">=3.7"));

            let _ = fs::remove_dir_all(&cache_dir);
        }

        #[tokio::test]
        async fn test_html_response_parsing() {
            let server = MockServer::start().await;
            let cache_dir = std::env::temp_dir().join("umbral-wiremock-html");
            let _ = fs::remove_dir_all(&cache_dir);

            let html_body = r#"<!DOCTYPE html>
<html><body>
<a href="https://files.example.com/pkg-1.0-py3-none-any.whl#sha256=abc" data-requires-python="&gt;=3.8">pkg-1.0-py3-none-any.whl</a>
</body></html>"#;

            Mock::given(method("GET"))
                .and(path("/simple/pkg/"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_string(html_body)
                        .insert_header("content-type", "text/html; charset=utf-8"),
                )
                .expect(1)
                .mount(&server)
                .await;

            let client = make_client(&server, &cache_dir);
            let page = client.fetch_project_page("pkg").await.unwrap();

            assert_eq!(page.files.len(), 1);
            assert_eq!(page.files[0].filename, "pkg-1.0-py3-none-any.whl");
            assert_eq!(page.files[0].requires_python.as_deref(), Some(">=3.8"));

            let _ = fs::remove_dir_all(&cache_dir);
        }

        #[tokio::test]
        async fn test_content_type_routing() {
            let server = MockServer::start().await;
            let cache_dir = std::env::temp_dir().join("umbral-wiremock-ct-routing");
            let _ = fs::remove_dir_all(&cache_dir);

            // Serve JSON with correct content-type
            let json_body = r#"{
                "name": "typed-pkg",
                "files": [
                    {
                        "filename": "typed_pkg-1.0-py3-none-any.whl",
                        "url": "https://files.example.com/typed_pkg-1.0.whl",
                        "hashes": {}
                    },
                    {
                        "filename": "typed_pkg-2.0-py3-none-any.whl",
                        "url": "https://files.example.com/typed_pkg-2.0.whl",
                        "hashes": {}
                    }
                ]
            }"#;

            Mock::given(method("GET"))
                .and(path("/simple/typed-pkg/"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_string(json_body)
                        .insert_header("content-type", "application/vnd.pypi.simple.v1+json"),
                )
                .expect(1)
                .mount(&server)
                .await;

            let client = make_client(&server, &cache_dir);
            let page = client.fetch_project_page("typed-pkg").await.unwrap();

            assert_eq!(page.files.len(), 2);
            assert_eq!(page.files[0].filename, "typed_pkg-1.0-py3-none-any.whl");
            assert_eq!(page.files[1].filename, "typed_pkg-2.0-py3-none-any.whl");

            let _ = fs::remove_dir_all(&cache_dir);
        }

        #[tokio::test]
        async fn test_retry_on_503() {
            let server = MockServer::start().await;
            let cache_dir = std::env::temp_dir().join("umbral-wiremock-retry");
            let _ = fs::remove_dir_all(&cache_dir);

            let json_body = r#"{
                "name": "flaky",
                "files": [
                    {
                        "filename": "flaky-1.0-py3-none-any.whl",
                        "url": "https://files.example.com/flaky-1.0.whl",
                        "hashes": {}
                    }
                ]
            }"#;

            // First two requests return 503, third succeeds.
            Mock::given(method("GET"))
                .and(path("/simple/flaky/"))
                .respond_with(ResponseTemplate::new(503))
                .expect(2)
                .up_to_n_times(2)
                .mount(&server)
                .await;

            Mock::given(method("GET"))
                .and(path("/simple/flaky/"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_string(json_body)
                        .insert_header("content-type", "application/vnd.pypi.simple.v1+json"),
                )
                .expect(1)
                .mount(&server)
                .await;

            let client = make_client(&server, &cache_dir);
            let page = client.fetch_project_page("flaky").await.unwrap();

            assert_eq!(page.files.len(), 1);
            assert_eq!(page.files[0].filename, "flaky-1.0-py3-none-any.whl");

            let _ = fs::remove_dir_all(&cache_dir);
        }

        #[tokio::test]
        async fn test_multi_index_fallback_on_404() {
            let primary_server = MockServer::start().await;
            let extra_server = MockServer::start().await;
            let cache_dir = std::env::temp_dir().join("umbral-wiremock-multi-index");
            let _ = fs::remove_dir_all(&cache_dir);

            // Primary returns 404 for "private-pkg".
            // The client retries on HTTP errors (1 initial + 3 retries = 4 total).
            Mock::given(method("GET"))
                .and(path("/simple/private-pkg/"))
                .respond_with(ResponseTemplate::new(404))
                .expect(4)
                .mount(&primary_server)
                .await;

            // Extra index returns the package
            let json_body = r#"{
                "name": "private-pkg",
                "files": [
                    {
                        "filename": "private_pkg-1.0.0-py3-none-any.whl",
                        "url": "https://files.example.com/private_pkg-1.0.0-py3-none-any.whl",
                        "hashes": {"sha256": "abc123"}
                    }
                ]
            }"#;

            Mock::given(method("GET"))
                .and(path("/extra/private-pkg/"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_string(json_body)
                        .insert_header("content-type", "application/vnd.pypi.simple.v1+json"),
                )
                .expect(1)
                .mount(&extra_server)
                .await;

            let primary_url = Url::parse(&format!("{}/simple/", primary_server.uri())).unwrap();
            let extra_url = Url::parse(&format!("{}/extra/", extra_server.uri())).unwrap();

            let client = SimpleApiClient::with_client_and_extras(
                Client::builder()
                    .timeout(Duration::from_secs(5))
                    .build()
                    .unwrap(),
                primary_url,
                vec![extra_url],
                cache_dir.to_path_buf(),
            );

            let page = client.fetch_project_page("private-pkg").await.unwrap();
            assert_eq!(page.files.len(), 1);
            assert_eq!(page.files[0].filename, "private_pkg-1.0.0-py3-none-any.whl");

            let _ = fs::remove_dir_all(&cache_dir);
        }

        #[tokio::test]
        async fn test_multi_index_primary_succeeds_no_fallback() {
            let primary_server = MockServer::start().await;
            let extra_server = MockServer::start().await;
            let cache_dir = std::env::temp_dir().join("umbral-wiremock-multi-index-primary-ok");
            let _ = fs::remove_dir_all(&cache_dir);

            // Primary has the package
            let json_body = r#"{
                "name": "requests",
                "files": [
                    {
                        "filename": "requests-2.31.0-py3-none-any.whl",
                        "url": "https://files.example.com/requests-2.31.0-py3-none-any.whl",
                        "hashes": {}
                    }
                ]
            }"#;

            Mock::given(method("GET"))
                .and(path("/simple/requests/"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_string(json_body)
                        .insert_header("content-type", "application/vnd.pypi.simple.v1+json"),
                )
                .expect(1)
                .mount(&primary_server)
                .await;

            // Extra should NOT be called
            Mock::given(method("GET"))
                .and(path("/extra/requests/"))
                .respond_with(ResponseTemplate::new(200))
                .expect(0)
                .mount(&extra_server)
                .await;

            let primary_url = Url::parse(&format!("{}/simple/", primary_server.uri())).unwrap();
            let extra_url = Url::parse(&format!("{}/extra/", extra_server.uri())).unwrap();

            let client = SimpleApiClient::with_client_and_extras(
                Client::builder()
                    .timeout(Duration::from_secs(5))
                    .build()
                    .unwrap(),
                primary_url,
                vec![extra_url],
                cache_dir.to_path_buf(),
            );

            let page = client.fetch_project_page("requests").await.unwrap();
            assert_eq!(page.files.len(), 1);

            let _ = fs::remove_dir_all(&cache_dir);
        }

        #[tokio::test]
        async fn test_cache_hit_304_not_modified() {
            let server = MockServer::start().await;
            let cache_dir = std::env::temp_dir().join("umbral-wiremock-304");
            let _ = fs::remove_dir_all(&cache_dir);

            let json_body = r#"{
                "name": "cached-pkg",
                "files": [
                    {
                        "filename": "cached_pkg-1.0-py3-none-any.whl",
                        "url": "https://files.example.com/cached_pkg-1.0.whl",
                        "hashes": {}
                    }
                ]
            }"#;

            // First request: 200 with ETag
            Mock::given(method("GET"))
                .and(path("/simple/cached-pkg/"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_string(json_body)
                        .insert_header("content-type", "application/vnd.pypi.simple.v1+json")
                        .insert_header("etag", "\"v1\""),
                )
                .expect(1)
                .up_to_n_times(1)
                .mount(&server)
                .await;

            let client = make_client(&server, &cache_dir);

            // First fetch — populates cache
            let page1 = client.fetch_project_page("cached-pkg").await.unwrap();
            assert_eq!(page1.files.len(), 1);

            // Now mount a 304 response for the second request (with If-None-Match)
            Mock::given(method("GET"))
                .and(path("/simple/cached-pkg/"))
                .and(header("if-none-match", "\"v1\""))
                .respond_with(ResponseTemplate::new(304))
                .expect(1)
                .mount(&server)
                .await;

            // Second fetch — should use cache via 304
            let page2 = client.fetch_project_page("cached-pkg").await.unwrap();
            assert_eq!(page2.files.len(), 1);
            assert_eq!(page2.files[0].filename, "cached_pkg-1.0-py3-none-any.whl");

            let _ = fs::remove_dir_all(&cache_dir);
        }
    }
}
