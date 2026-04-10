use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Args;
use miette::{IntoDiagnostic, WrapErr};
use tracing::{info, warn};

#[derive(Debug, Args)]
pub struct PublishArgs {
    /// Distribution files to upload (default: dist/*)
    #[arg(default_value = "dist")]
    pub dist: PathBuf,

    /// PyPI token for authentication
    #[arg(long, env = "UMBRAL_PUBLISH_TOKEN")]
    pub token: Option<String>,

    /// Repository URL to publish to
    #[arg(long, default_value = "https://upload.pypi.org/legacy/")]
    pub repository: String,

    /// Skip existing files (don't error if already uploaded)
    #[arg(long)]
    pub skip_existing: bool,
}

pub fn cmd_publish(args: PublishArgs) -> miette::Result<()> {
    // 1. Find distribution files
    let dist_files = find_distributions(&args.dist)?;
    if dist_files.is_empty() {
        return Err(miette::miette!(
            "no distribution files found in {}. Run `umbral build` first.",
            args.dist.display()
        ));
    }

    // 2. Get authentication token
    let token = args
        .token
        .or_else(|| std::env::var("UMBRAL_PUBLISH_TOKEN").ok())
        .or_else(|| std::env::var("UV_PUBLISH_TOKEN").ok())
        .ok_or_else(|| {
            miette::miette!(
                "no authentication token provided. Use --token, UMBRAL_PUBLISH_TOKEN, or UV_PUBLISH_TOKEN"
            )
        })?;

    // 3. Upload each file
    let rt = tokio::runtime::Runtime::new().into_diagnostic()?;
    for file in &dist_files {
        eprintln!(
            "Uploading {}...",
            file.file_name().unwrap_or_default().to_string_lossy()
        );
        rt.block_on(upload_distribution(
            file,
            &args.repository,
            &token,
            args.skip_existing,
        ))?;
    }

    eprintln!(
        "Published {} file(s) to {}",
        dist_files.len(),
        args.repository
    );
    Ok(())
}

fn find_distributions(path: &Path) -> miette::Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    if path.is_file() {
        // Single file specified
        files.push(path.to_path_buf());
    } else if path.is_dir() {
        // Scan directory for .whl and .tar.gz files
        for entry in std::fs::read_dir(path).into_diagnostic()? {
            let entry = entry.into_diagnostic()?;
            let path = entry.path();
            let name = path.to_string_lossy().to_string();
            if name.ends_with(".whl") || name.ends_with(".tar.gz") {
                files.push(path);
            }
        }
        files.sort(); // deterministic order
    }

    Ok(files)
}

async fn upload_distribution(
    file_path: &Path,
    repository_url: &str,
    token: &str,
    skip_existing: bool,
) -> miette::Result<()> {
    use reqwest::multipart;

    let file_name = file_path
        .file_name()
        .ok_or_else(|| miette::miette!("invalid file path"))?
        .to_string_lossy()
        .to_string();

    // Read file content
    let content = std::fs::read(file_path)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read {}", file_path.display()))?;

    // Compute SHA-256 for PyPI
    let sha256_digest = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&content);
        hex::encode(hasher.finalize())
    };

    // Determine filetype
    let filetype = if file_name.ends_with(".whl") {
        "bdist_wheel"
    } else {
        "sdist"
    };

    // Extract metadata from filename
    let (name, version) = extract_name_version(&file_name)?;

    // Build multipart form
    let file_part = multipart::Part::bytes(content)
        .file_name(file_name.clone())
        .mime_str("application/octet-stream")
        .into_diagnostic()?;

    let form = multipart::Form::new()
        .text(":action", "file_upload")
        .text("protocol_version", "1")
        .text("name", name.clone())
        .text("version", version.clone())
        .text("filetype", filetype.to_string())
        .text("sha256_digest", sha256_digest.clone())
        .part("content", file_part);

    // Upload with authentication (timeouts configurable via UMBRAL_HTTP_TIMEOUT)
    let timeout_secs = std::env::var("UMBRAL_HTTP_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(120);
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .into_diagnostic()?;

    // First attempt
    let response = client
        .post(repository_url)
        .basic_auth("__token__", Some(token))
        .multipart(form)
        .send()
        .await;

    // Retry once on 5xx or connection failure after 1s backoff
    let response = match &response {
        Ok(resp) if resp.status().is_server_error() => {
            warn!(status = %resp.status(), "upload got 5xx, retrying once after 1s");
            tokio::time::sleep(Duration::from_secs(1)).await;

            // Rebuild the multipart form for retry (the original was consumed)
            let retry_content = std::fs::read(file_path)
                .into_diagnostic()
                .wrap_err("failed to re-read file for retry")?;
            let retry_file_part = multipart::Part::bytes(retry_content)
                .file_name(file_name.clone())
                .mime_str("application/octet-stream")
                .into_diagnostic()?;
            let retry_form = multipart::Form::new()
                .text(":action", "file_upload")
                .text("protocol_version", "1")
                .text("name", name.clone())
                .text("version", version.clone())
                .text("filetype", filetype.to_string())
                .text("sha256_digest", sha256_digest.clone())
                .part("content", retry_file_part);

            client
                .post(repository_url)
                .basic_auth("__token__", Some(token))
                .multipart(retry_form)
                .send()
                .await
                .into_diagnostic()
                .wrap_err("upload retry request failed")?
        }
        Err(_) => {
            // Connection failure — retry once after 1s
            warn!("upload connection failed, retrying once after 1s");
            tokio::time::sleep(Duration::from_secs(1)).await;

            let retry_content = std::fs::read(file_path)
                .into_diagnostic()
                .wrap_err("failed to re-read file for retry")?;
            let retry_file_part = multipart::Part::bytes(retry_content)
                .file_name(file_name.clone())
                .mime_str("application/octet-stream")
                .into_diagnostic()?;
            let retry_form = multipart::Form::new()
                .text(":action", "file_upload")
                .text("protocol_version", "1")
                .text("name", name.clone())
                .text("version", version.clone())
                .text("filetype", filetype.to_string())
                .text("sha256_digest", sha256_digest.clone())
                .part("content", retry_file_part);

            client
                .post(repository_url)
                .basic_auth("__token__", Some(token))
                .multipart(retry_form)
                .send()
                .await
                .into_diagnostic()
                .wrap_err("upload retry request failed")?
        }
        Ok(_) => response
            .into_diagnostic()
            .wrap_err("upload request failed")?,
    };

    let status = response.status();

    if status.is_success() {
        info!("uploaded {}", file_name);
        Ok(())
    } else if skip_existing && status.as_u16() == 409 {
        eprintln!("Skipping {} (already exists)", file_name);
        Ok(())
    } else {
        let body = response.text().await.unwrap_or_default();
        // PyPI returns 400 for "already exists" too
        if skip_existing && body.contains("already exists") {
            eprintln!("Skipping {} (already exists)", file_name);
            Ok(())
        } else {
            Err(miette::miette!(
                "upload failed for {} (HTTP {}): {}",
                file_name,
                status,
                body
            ))
        }
    }
}

fn extract_name_version(filename: &str) -> miette::Result<(String, String)> {
    if filename.ends_with(".whl") {
        // PEP 427: {distribution}-{version}(-{build})?-{python}-{abi}-{platform}.whl
        let stem = filename.trim_end_matches(".whl");
        let parts: Vec<&str> = stem.split('-').collect();
        // Minimum valid wheel: name-version-python-abi-platform = 5 parts
        if parts.len() < 5 {
            return Err(miette::miette!(
                "invalid wheel filename (too few segments): {}",
                filename
            ));
        }
        // Last 3 segments are always python-abi-platform
        let name_version_parts = &parts[..parts.len() - 3];
        if name_version_parts.len() < 2 {
            return Err(miette::miette!(
                "cannot extract name and version from wheel: {}",
                filename
            ));
        }
        // Check for optional build tag: if the last element is purely numeric, it's a build tag
        let (version_idx, _build_tag) = if name_version_parts
            .last()
            .expect("name_version_parts has at least 2 elements")
            .chars()
            .all(|c| c.is_ascii_digit())
            && name_version_parts.len() >= 3
        {
            // Build tag present: version is second-to-last
            (name_version_parts.len() - 2, true)
        } else {
            (name_version_parts.len() - 1, false)
        };
        let version = name_version_parts[version_idx].to_string();
        let name = name_version_parts[..version_idx].join("-");
        Ok((name.replace('_', "-"), version))
    } else if filename.ends_with(".tar.gz") {
        // sdist: name-version.tar.gz
        let stem = filename.trim_end_matches(".tar.gz");
        if let Some(pos) = stem.rfind('-') {
            Ok((stem[..pos].to_string(), stem[pos + 1..].to_string()))
        } else {
            Err(miette::miette!("cannot parse sdist filename: {}", filename))
        }
    } else {
        Err(miette::miette!("unknown distribution format: {}", filename))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_name_version_simple_wheel() {
        let (name, version) = extract_name_version("simple-1.0.0-py3-none-any.whl").unwrap();
        assert_eq!(name, "simple");
        assert_eq!(version, "1.0.0");
    }

    #[test]
    fn test_extract_name_version_hyphenated_wheel() {
        let (name, version) = extract_name_version("my-package-1.0.0-py3-none-any.whl").unwrap();
        assert_eq!(name, "my-package");
        assert_eq!(version, "1.0.0");
    }

    #[test]
    fn test_extract_name_version_underscore_wheel() {
        let (name, version) =
            extract_name_version("my_package-2.0.0-cp312-cp312-linux_x86_64.whl").unwrap();
        assert_eq!(name, "my-package");
        assert_eq!(version, "2.0.0");
    }

    #[test]
    fn test_extract_name_version_build_tag() {
        let (name, version) =
            extract_name_version("foo-2.0.0-1-cp312-cp312-linux_x86_64.whl").unwrap();
        assert_eq!(name, "foo");
        assert_eq!(version, "2.0.0");
    }

    #[test]
    fn test_extract_name_version_simple_sdist() {
        let (name, version) = extract_name_version("mypackage-1.0.0.tar.gz").unwrap();
        assert_eq!(name, "mypackage");
        assert_eq!(version, "1.0.0");
    }

    #[test]
    fn test_extract_name_version_hyphenated_sdist() {
        let (name, version) = extract_name_version("my-package-1.0.0.tar.gz").unwrap();
        assert_eq!(name, "my-package");
        assert_eq!(version, "1.0.0");
    }

    #[test]
    fn test_extract_name_version_unknown_format() {
        let result = extract_name_version("mypackage-1.0.0.zip");
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_name_version_multi_hyphen_wheel() {
        let (name, version) =
            extract_name_version("my-cool-package-2.3.1-py3-none-any.whl").unwrap();
        assert_eq!(name, "my-cool-package");
        assert_eq!(version, "2.3.1");
    }

    #[test]
    fn test_extract_name_version_underscore_multi_wheel() {
        let (name, version) =
            extract_name_version("my_cool_package-2.3.1-py3-none-any.whl").unwrap();
        assert_eq!(name, "my-cool-package");
        assert_eq!(version, "2.3.1");
    }

    #[test]
    fn test_find_distributions_in_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        // Create test files
        std::fs::write(dir.join("pkg-1.0.0-py3-none-any.whl"), b"wheel").unwrap();
        std::fs::write(dir.join("pkg-1.0.0.tar.gz"), b"sdist").unwrap();
        std::fs::write(dir.join("readme.txt"), b"text").unwrap();
        std::fs::write(dir.join("notes.md"), b"markdown").unwrap();

        let files = find_distributions(dir).unwrap();
        assert_eq!(files.len(), 2);

        let names: Vec<String> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&"pkg-1.0.0-py3-none-any.whl".to_string()));
        assert!(names.contains(&"pkg-1.0.0.tar.gz".to_string()));
    }

    #[test]
    fn test_find_distributions_single_file() {
        let tmp = tempfile::tempdir().unwrap();
        let whl = tmp.path().join("pkg-1.0.0-py3-none-any.whl");
        std::fs::write(&whl, b"wheel").unwrap();

        let files = find_distributions(&whl).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], whl);
    }

    #[test]
    fn test_find_distributions_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let files = find_distributions(tmp.path()).unwrap();
        assert!(files.is_empty());
    }
}
