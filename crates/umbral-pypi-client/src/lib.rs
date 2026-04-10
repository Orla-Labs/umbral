//! Async PyPI Simple API client with PEP 503/691/658 support.
//!
//! This crate provides [`SimpleApiClient`] for querying Python package indices
//! (PyPI and compatible registries) using the Simple Repository API. It supports:
//!
//! - **PEP 503**: HTML Simple Repository API
//! - **PEP 691**: JSON Simple Repository API (preferred via content negotiation)
//! - **PEP 658**: Direct metadata access (`{url}.metadata`)
//! - Disk caching with ETag/Last-Modified conditional requests
//! - Automatic retries with exponential backoff

pub mod cache;
pub mod client;
pub mod error;
pub mod html;
pub mod json;
pub mod metadata;
pub mod tags;
pub mod wheel;

use std::collections::HashMap;

pub use client::SimpleApiClient;
pub use error::{PypiClientError, Result};
pub use metadata::Metadata;
pub use tags::{PlatformTags, WheelTag};
pub use wheel::WheelFilename;

/// A parsed project page from the Simple API containing distribution files.
#[derive(Debug, Clone)]
pub struct ProjectPage {
    pub files: Vec<DistributionFile>,
}

/// A single distribution file listed on a project page.
#[derive(Debug, Clone)]
pub struct DistributionFile {
    /// The filename (e.g., `requests-2.31.0-py3-none-any.whl`).
    pub filename: String,
    /// The download URL.
    pub url: String,
    /// Hash digests keyed by algorithm (e.g., `{"sha256": "abcdef..."}`).
    pub hashes: HashMap<String, String>,
    /// PEP 503 `data-requires-python` (e.g., `>=3.7`).
    pub requires_python: Option<String>,
    /// PEP 658 `data-dist-info-metadata` — signals that `.metadata` is available.
    pub dist_info_metadata: Option<String>,
    /// If set, the distribution has been yanked. The value is the reason (may be empty).
    pub yanked: Option<String>,
}
