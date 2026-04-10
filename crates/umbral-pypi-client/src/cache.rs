//! Disk-based HTTP response cache with ETag/Last-Modified support.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::error::Result;

/// Cached HTTP response metadata for conditional requests.
#[derive(Debug, Serialize, Deserialize)]
pub struct CacheEntry {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// A simple disk cache that stores response bodies alongside their HTTP metadata.
///
/// Each entry consists of two files:
/// - `{key}.data` — the response body
/// - `{key}.meta` — JSON-serialized `CacheEntry` with ETag/Last-Modified
pub struct DiskCache {
    cache_dir: PathBuf,
}

impl DiskCache {
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            cache_dir: cache_dir.into(),
        }
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    fn entry_path(&self, key: &str, extension: &str) -> PathBuf {
        // Replace `/` in keys with `_` to create flat file names
        let safe_key = key.replace('/', "_");
        self.cache_dir.join(format!("{safe_key}.{extension}"))
    }

    /// Read cached content and metadata, if present.
    pub fn read(&self, key: &str) -> Option<(String, CacheEntry)> {
        let content_path = self.entry_path(key, "data");
        let meta_path = self.entry_path(key, "meta");

        let content = std::fs::read_to_string(&content_path).ok()?;
        let meta_str = std::fs::read_to_string(&meta_path).ok()?;
        let entry: CacheEntry = serde_json::from_str(&meta_str).ok()?;

        debug!(key, "cache hit");
        Some((content, entry))
    }

    /// Write content and metadata to the cache.
    pub fn write(&self, key: &str, content: &str, entry: &CacheEntry) -> Result<()> {
        std::fs::create_dir_all(&self.cache_dir)?;

        let content_path = self.entry_path(key, "data");
        let meta_path = self.entry_path(key, "meta");

        std::fs::write(&content_path, content)?;
        std::fs::write(&meta_path, serde_json::to_string(entry)?)?;

        debug!(key, "cached response");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_cache_roundtrip() {
        let dir = std::env::temp_dir().join("umbral-cache-test-roundtrip");
        let _ = fs::remove_dir_all(&dir);

        let cache = DiskCache::new(&dir);

        let entry = CacheEntry {
            etag: Some("\"abc123\"".to_string()),
            last_modified: Some("Mon, 01 Jan 2024 00:00:00 GMT".to_string()),
        };

        cache.write("test-key", "hello world", &entry).unwrap();

        let (content, meta) = cache.read("test-key").unwrap();
        assert_eq!(content, "hello world");
        assert_eq!(meta.etag.as_deref(), Some("\"abc123\""));
        assert_eq!(
            meta.last_modified.as_deref(),
            Some("Mon, 01 Jan 2024 00:00:00 GMT")
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_cache_miss() {
        let dir = std::env::temp_dir().join("umbral-cache-test-miss");
        let _ = fs::remove_dir_all(&dir);

        let cache = DiskCache::new(&dir);
        assert!(cache.read("nonexistent").is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_key_with_slashes() {
        let dir = std::env::temp_dir().join("umbral-cache-test-slashes");
        let _ = fs::remove_dir_all(&dir);

        let cache = DiskCache::new(&dir);
        let entry = CacheEntry {
            etag: None,
            last_modified: None,
        };

        cache.write("projects/requests", "data", &entry).unwrap();
        let (content, _) = cache.read("projects/requests").unwrap();
        assert_eq!(content, "data");

        let _ = fs::remove_dir_all(&dir);
    }
}
