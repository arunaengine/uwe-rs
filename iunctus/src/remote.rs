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
        "http" | "https" => Some(raw_github(&uri).unwrap_or(uri)),
        "s3" => {
            let rest = uri.strip_prefix("s3://")?;
            let (bucket, key) = rest.split_once('/').unwrap_or((rest, ""));
            Some(format!("https://{bucket}.s3.amazonaws.com/{key}"))
        }
        _ => None,
    }
}

/// The address that GitHub redirects a file of a repository to: `github.com/o/r/raw/ref/path`
/// is served by `raw.githubusercontent.com/o/r/ref/path`. Going there directly saves the
/// redirect, and works where only the file host is reachable.
fn raw_github(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("https://github.com/")?;
    let mut parts = rest.splitn(4, '/');
    let (owner, repository) = (parts.next()?, parts.next()?);
    let (raw, file) = (parts.next()?, parts.next()?);
    (raw == "raw" && !file.is_empty())
        .then(|| format!("https://raw.githubusercontent.com/{owner}/{repository}/{file}"))
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
    let local = downloads.join(&name);
    if local.is_file() {
        return Ok(local);
    }
    std::fs::create_dir_all(downloads).map_err(|error| error.to_string())?;
    // The body goes to a file as it comes, which is renamed once complete. The name of the
    // file has the process id, so that runs that download the same file at once do not mix.
    // A run that was stopped leaves its part, which the next run takes over and continues.
    let partial = local.with_extension(format!("{}.partial", std::process::id()));
    let present = take_over(downloads, &name, &partial);
    let cannot = |error: &dyn std::fmt::Display| format!("Cannot read {url}: {error}");
    let mut request = download_client()?.get(url);
    if present > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={present}-"));
    }
    let mut response = request
        .send()
        .and_then(|response| response.error_for_status())
        .map_err(|error| cannot(&error))?;
    let resumed = response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resumed)
        .truncate(!resumed)
        .open(&partial)
        .map_err(|error| error.to_string())?;
    response
        .copy_to(&mut file)
        .map_err(|error| cannot(&error))?;
    std::fs::rename(&partial, &local).map_err(|error| error.to_string())?;
    Ok(local)
}

/// Takes over the largest part of `name` that an earlier run left in `downloads` by renaming
/// it to `partial`, and gives its size; 0 when there is none. A part of a run that still
/// goes on, `<name>.<process id>.partial`, is left alone. Of two runs that try at once, only
/// one can rename a part.
fn take_over(downloads: &Path, name: &str, partial: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(downloads) else {
        return 0;
    };
    let left = |file: &str| {
        let Some(middle) = file
            .strip_prefix(&format!("{name}."))
            .and_then(|rest| rest.strip_suffix("partial"))
        else {
            return false;
        };
        // The part of a stopped run, or of the first version of this function without an id.
        let owner = middle.trim_end_matches('.');
        owner.is_empty() || !Path::new("/proc").join(owner).exists()
    };
    let mut parts: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter(|entry| left(&entry.file_name().to_string_lossy()))
        .filter_map(|entry| Some((entry.metadata().ok()?.len(), entry.path())))
        .collect();
    parts.sort();
    while let Some((size, part)) = parts.pop() {
        if std::fs::rename(&part, partial).is_ok() {
            return size;
        }
    }
    0
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
    fn stopped_downloads_are_taken_over() {
        let downloads = tempfile::tempdir().expect("a temporary directory");
        let part = |name: &str, size: usize| {
            std::fs::write(downloads.path().join(name), vec![b'x'; size]).expect("writes");
        };
        // A part of this process, which runs, and parts of stopped runs.
        part(&format!("abc.{}.partial", std::process::id()), 9);
        part("abc.4294967295.partial", 5);
        part("abc.partial", 3);
        part("other.4294967295.partial", 7);
        let own = downloads.path().join("abc.1.partial");
        assert_eq!(take_over(downloads.path(), "abc", &own), 5);
        assert!(own.is_file());
        assert_eq!(take_over(downloads.path(), "abc", &own), 3);
        assert_eq!(take_over(downloads.path(), "abc", &own), 0);
    }

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
        assert_eq!(
            url("https://github.com/nf-core/test-datasets/raw/methylseq/testdata/a.fastq.gz")
                .as_deref(),
            Some(
                "https://raw.githubusercontent.com/nf-core/test-datasets/methylseq/testdata/a.fastq.gz"
            )
        );
        assert_eq!(
            url("https://github.com/nf-core/modules/blob/master/README.md").as_deref(),
            Some("https://github.com/nf-core/modules/blob/master/README.md")
        );
    }
}
