//! Remote inputs: files at `http://` and `https://` addresses and in public `s3://` and
//! `gs://` buckets, which are read over HTTPS as anonymous clients do. Each is downloaded once
//! into a folder of downloads, under the SHA-256 of its address, and later runs take it from
//! there.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};
use tokio::io::AsyncWriteExt;

use crate::RunnerError;
use crate::runner::at;

/// The HTTPS address of a remote file: `http` and `https` as they are, `s3://bucket/key` and
/// `gs://bucket/key` at the bucket's public address. `None` for other schemes.
pub fn https_url(source: &str) -> Option<String> {
    let (scheme, rest) = source.split_once("://")?;
    match scheme {
        "http" | "https" => Some(raw_github(source).unwrap_or_else(|| source.to_owned())),
        "s3" => {
            let (bucket, key) = rest.split_once('/').unwrap_or((rest, ""));
            Some(format!("https://{bucket}.s3.amazonaws.com/{key}"))
        }
        "gs" => Some(format!("https://storage.googleapis.com/{rest}")),
        _ => None,
    }
}

/// The address that GitHub redirects a file of a repository to: `github.com/o/r/raw/ref/path`
/// is served by `raw.githubusercontent.com/o/r/ref/path`.
fn raw_github(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://github.com/")?;
    let mut parts = rest.splitn(4, '/');
    let (owner, repository) = (parts.next()?, parts.next()?);
    let (raw, file) = (parts.next()?, parts.next()?);
    (raw == "raw" && !file.is_empty())
        .then(|| format!("https://raw.githubusercontent.com/{owner}/{repository}/{file}"))
}

/// A local copy of a remote file in `downloads`, downloaded unless an earlier run did. The
/// body goes to a file of this process as it comes, which is renamed once complete, so that
/// runs that download the same file at once do not mix.
pub async fn download(source: &str, downloads: &Path) -> Result<PathBuf, RunnerError> {
    let url = https_url(source).ok_or_else(|| RunnerError::Remote(source.to_owned()))?;
    let name: String = (Sha256::digest(url.as_bytes()).iter())
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let local = downloads.join(&name);
    if tokio::fs::try_exists(&local).await.map_err(at(&local))? {
        return Ok(local);
    }
    (tokio::fs::create_dir_all(downloads).await).map_err(at(downloads))?;
    let failed = |error: reqwest::Error| RunnerError::Download {
        url: url.clone(),
        message: error.to_string(),
    };
    let client = executor::http::client().map_err(|message| RunnerError::Download {
        url: url.clone(),
        message,
    })?;
    let mut response = (client.get(&url).send().await)
        .and_then(|response| response.error_for_status())
        .map_err(failed)?;
    let partial = local.with_extension(format!("{}.partial", std::process::id()));
    let mut file = (tokio::fs::File::create(&partial).await).map_err(at(&partial))?;
    while let Some(chunk) = response.chunk().await.map_err(failed)? {
        file.write_all(&chunk).await.map_err(at(&partial))?;
    }
    file.flush().await.map_err(at(&partial))?;
    drop(file);
    (tokio::fs::rename(&partial, &local).await).map_err(at(&local))?;
    Ok(local)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_files_have_https_addresses() {
        assert_eq!(
            https_url("s3://ngi-igenomes/test-data/x.zip").as_deref(),
            Some("https://ngi-igenomes.s3.amazonaws.com/test-data/x.zip")
        );
        assert_eq!(
            https_url("gs://deepvariant/GIAB_v5q0/x.bed").as_deref(),
            Some("https://storage.googleapis.com/deepvariant/GIAB_v5q0/x.bed")
        );
        assert_eq!(
            https_url("https://github.com/nf-core/test-datasets/raw/demo/a.fastq.gz").as_deref(),
            Some("https://raw.githubusercontent.com/nf-core/test-datasets/demo/a.fastq.gz")
        );
        assert_eq!(https_url("ftp://host/file"), None);
    }
}
