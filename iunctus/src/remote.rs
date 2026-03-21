//! Remote files over HTTPS: downloads into a cache, and whether a file exists without a
//! download. Public S3 buckets are read over HTTPS, as anonymous clients do.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use nextflow_evaluator::interpreter::FileMetadata;
use nextflow_evaluator::value::PathValue;
use sha2::{Digest, Sha256};

/// The HTTPS address of a remote file: `http` and `https` as they are, `s3://bucket/key` at
/// the bucket's public address. `None` for other schemes.
pub fn https_url(path: &PathValue) -> Option<String> {
    let uri = path.uri();
    match path.scheme() {
        "http" | "https" => Some(uri),
        "s3" => {
            let rest = uri.strip_prefix("s3://")?;
            let (bucket, key) = rest.split_once('/').unwrap_or((rest, ""));
            Some(format!("https://{bucket}.s3.amazonaws.com/{key}"))
        }
        _ => None,
    }
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent(concat!("iunctus/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| error.to_string())
}

/// A client for downloads, which may take long for large files.
fn download_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent(concat!("iunctus/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(30 * 60))
        .build()
        .map_err(|error| error.to_string())
}

/// A local copy of a remote file, downloaded once into `downloads`.
pub fn download(url: &str, downloads: &Path) -> Result<PathBuf, String> {
    let digest = Sha256::digest(url.as_bytes());
    let name: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    let local = downloads.join(name);
    if local.is_file() {
        return Ok(local);
    }
    let cannot = |error: &dyn std::fmt::Display| format!("Cannot read {url}: {error}");
    let mut response = download_client()?
        .get(url)
        .send()
        .and_then(|response| response.error_for_status())
        .map_err(|error| cannot(&error))?;
    std::fs::create_dir_all(downloads).map_err(|error| error.to_string())?;
    // The body goes to a file as it comes, which is renamed once complete.
    let partial = local.with_extension("partial");
    let mut file = std::fs::File::create(&partial).map_err(|error| error.to_string())?;
    response.copy_to(&mut file).map_err(|error| cannot(&error))?;
    std::fs::rename(&partial, &local).map_err(|error| error.to_string())?;
    Ok(local)
}

/// Whether a remote file exists, and its size, from a `HEAD` request. A key of an S3 bucket
/// that is a prefix of other keys is a directory.
pub fn metadata(path: &PathValue, url: &str) -> Result<Option<FileMetadata>, String> {
    let client = client()?;
    let response = client
        .head(url)
        .send()
        .map_err(|error| format!("Cannot read {url}: {error}"))?;
    if response.status().is_success() {
        // The header gives the size of the file; the response itself has no body.
        let size = response
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|length| length.to_str().ok()?.parse().ok())
            .unwrap_or(0);
        return Ok(Some(FileMetadata {
            directory: false,
            size,
        }));
    }
    if path.scheme() == "s3" {
        let (bucket, key) = url
            .strip_prefix("https://")
            .and_then(|rest| rest.split_once(".s3.amazonaws.com/"))
            .unwrap_or_default();
        let prefix = format!("{}/", key.trim_end_matches('/'));
        let listing =
            format!("https://{bucket}.s3.amazonaws.com/?list-type=2&max-keys=1&prefix={prefix}");
        let text = client
            .get(&listing)
            .send()
            .and_then(|response| response.text())
            .map_err(|error| format!("Cannot list {listing}: {error}"))?;
        if text.contains("<Key>") {
            return Ok(Some(FileMetadata {
                directory: true,
                size: 0,
            }));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_files_have_https_addresses() {
        let url = |text: &str| https_url(&PathValue::parse(text, "/"));
        assert_eq!(
            url("s3://ngi-igenomes/test-data/x.zip").as_deref(),
            Some("https://ngi-igenomes.s3.amazonaws.com/test-data/x.zip")
        );
        assert_eq!(
            url("https://zenodo.org/records/1/files/a.csv").as_deref(),
            Some("https://zenodo.org/records/1/files/a.csv")
        );
        assert_eq!(url("/local/file.txt"), None);
    }
}
