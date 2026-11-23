//! Begins S3 multipart uploads for large output files and presigns their parts for the helper.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::{Duration, SystemTime};

use crate::helper::{LEAST_PART, MOST_BYTES, Parts, Request};
use crate::job::ExecutorError;

/// How an S3 staging root takes output files above the size of one upload: in parts that this
/// machine presigns for each attempt, so that the workers hold no credentials.
#[derive(Clone, Debug)]
pub struct Multipart {
    /// The address of the S3 service, like `https://s3.eu-central-1.amazonaws.com`.
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    /// The key of the staging root in the bucket, without a slash at either end.
    pub root: String,
    /// Whether the bucket is part of the host name, as AWS prefers, rather than of the path.
    pub virtual_host: bool,
    pub keys: storage::aws::Keys,
    /// How many output files of one attempt may go up in parts.
    pub files: usize,
    /// How many parts each of them may have: 64 parts of 5 GiB hold 320 GiB.
    pub parts: usize,
    /// The size in bytes above which an output file goes up in parts.
    pub threshold: u64,
    /// The least size in bytes of a part but the last, [`LEAST_PART`] as S3 demands.
    pub least_part: u64,
}

/// What a single upload may take with a margin, so that a file close to the limit of S3 goes up
/// in parts rather than fail.
pub const THRESHOLD: u64 = MOST_BYTES - (64 << 20);

impl Multipart {
    /// Multipart uploads into the staging root `root`, like `s3://bucket/prefix`, through the S3
    /// service at `endpoint`, with the default number of files, parts and threshold.
    pub fn new(
        endpoint: &str,
        region: &str,
        root: &str,
        keys: storage::aws::Keys,
    ) -> Result<Multipart, String> {
        let rest = root
            .strip_prefix("s3://")
            .ok_or_else(|| format!("multipart staging needs an s3:// root, not {root}"))?;
        let (bucket, key) = rest.split_once('/').unwrap_or((rest, ""));
        if bucket.is_empty() {
            return Err(format!("the staging root {root} names no bucket"));
        }
        Ok(Multipart {
            endpoint: endpoint.trim_end_matches('/').to_owned(),
            region: region.to_owned(),
            bucket: bucket.to_owned(),
            root: key.trim_matches('/').to_owned(),
            virtual_host: false,
            keys,
            files: 4,
            parts: 64,
            threshold: THRESHOLD,
            least_part: LEAST_PART,
        })
    }

    /// Begins one multipart upload for each large file that an attempt at `prefix` may send,
    /// at `large/<slot>` below the prefix, and presigns its parts, its completion and its abort.
    /// A failure aborts the uploads that began before it, so that the bucket keeps no parts.
    pub async fn begin(&self, prefix: &str, expiry: Duration) -> Result<Vec<Parts>, ExecutorError> {
        let client = crate::http::client().map_err(failed)?;
        let mut slots = Vec::with_capacity(self.files);
        for slot in 1..=self.files {
            match self.slot(client, prefix, slot, expiry).await {
                Ok(parts) => slots.push(parts),
                Err(error) => {
                    abort(&slots).await;
                    return Err(error);
                }
            }
        }
        Ok(slots)
    }

    /// Begins the multipart upload of the slot `slot` of an attempt at `prefix`.
    async fn slot(
        &self,
        client: &reqwest::Client,
        prefix: &str,
        slot: usize,
        expiry: Duration,
    ) -> Result<Parts, ExecutorError> {
        let name = format!("large/{slot}");
        let key = [self.root.as_str(), prefix, &name]
            .iter()
            .filter(|part| !part.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join("/");
        let create = self.presigned("POST", &key, "uploads", Duration::from_secs(300))?;
        // The message of reqwest names the presigned URL, whose signature grants access.
        let hidden = |error: reqwest::Error| failed(error.without_url());
        let response = client.post(&create.url).send().await.map_err(hidden)?;
        let status = response.status();
        let text = response.text().await.map_err(hidden)?;
        let upload = between(&text, "<UploadId>", "</UploadId>")
            .filter(|_| status.is_success())
            .ok_or_else(|| failed(format!("S3 began no multipart upload ({status})")))?;
        let upload = encoded(upload);
        let mut uploads = Vec::with_capacity(self.parts);
        for number in 1..=self.parts {
            let query = format!("partNumber={number}&uploadId={upload}");
            uploads.push(self.presigned("PUT", &key, &query, expiry)?);
        }
        let query = format!("uploadId={upload}");
        Ok(Parts {
            name,
            uploads,
            complete: self.presigned("POST", &key, &query, expiry)?,
            abort: self.presigned("DELETE", &key, &query, expiry)?,
        })
    }

    /// A presigned request of `method` for the object at `key` with the query `query`.
    fn presigned(
        &self,
        method: &str,
        key: &str,
        query: &str,
        expiry: Duration,
    ) -> Result<Request, ExecutorError> {
        let path = encoded(key).replace("%2F", "/");
        let url = match self.virtual_host {
            true => {
                let (scheme, host) = self.endpoint.split_once("://").unwrap_or(("https", ""));
                format!("{scheme}://{}.{host}/{path}?{query}", self.bucket)
            }
            false => format!("{}/{}/{path}?{query}", self.endpoint, self.bucket),
        };
        let request = storage::aws::Request {
            service: "s3".into(),
            region: self.region.clone(),
            method: method.into(),
            url,
            headers: Vec::new(),
            body: Vec::new(),
        };
        let url = storage::aws::presign(&request, &self.keys, SystemTime::now(), expiry.as_secs())
            .map_err(failed)?;
        Ok(Request {
            method: method.into(),
            url,
            headers: Vec::new(),
        })
    }
}

/// Aborts these multipart uploads; one that is complete or gone already stays as it is. A
/// failure leaves parts in the bucket, which only a warning can tell.
pub async fn abort(slots: &[Parts]) {
    let Ok(client) = crate::http::client() else {
        return;
    };
    for slot in slots {
        let sent = client.delete(&slot.abort.url).send().await;
        if let Err(error) = sent.and_then(reqwest::Response::error_for_status) {
            let error = error.without_url();
            tracing::warn!(slot = slot.name, %error, "cannot abort a multipart upload");
        }
    }
}

/// The text between the first `start` and the next `end`.
pub(crate) fn between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let (_, rest) = text.split_once(start)?;
    rest.split_once(end).map(|(found, _)| found)
}

/// Percent-encodes all but the unreserved characters of RFC 3986.
fn encoded(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn failed(error: impl std::fmt::Display) -> ExecutorError {
    std::io::Error::other(format!("multipart staging failed: {error}")).into()
}

#[cfg(test)]
#[path = "multipart_tests.rs"]
mod tests;
