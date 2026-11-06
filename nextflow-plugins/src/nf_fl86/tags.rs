//! Keeps the S3 object tags of a source when nf-fl86's run publishes it to another S3 object:
//! `GetObjectTagging` on both, the merged tags with `PutObjectTagging` on the target.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use base64::Engine;
use md5::{Digest, Md5};
use storage::aws;

use super::secrets::Access;

/// The most tags an S3 object may have.
const LIMIT: usize = 10;

/// Where S3 requests go: the endpoint of `aws.client.endpoint` with path-style addresses, else
/// the regional virtual-hosted endpoint.
#[derive(Clone, Debug)]
pub(crate) struct Store {
    pub access: Access,
    pub endpoint: Option<String>,
}

/// Why tags were not kept, in the plugin's words.
struct Failure {
    stage: &'static str,
    kind: &'static str,
    status: Option<u16>,
    code: Option<String>,
}

/// Copies the tags of `source` onto `target`; an error is the message that fails the run, a
/// destination that refuses tag reads only warns.
pub(crate) fn keep(store: &Store, source: &str, target: &str) -> Result<(), String> {
    let message = |failure: &Failure, action: &str| {
        let mut text = format!(
            "Failed to preserve source S3 tags on published output; stage={}; source={}; \
             destination={}; errorType={}",
            failure.stage,
            shown(source),
            shown(target),
            failure.kind
        );
        if let Some(status) = failure.status {
            text.push_str(&format!("; statusCode={status}"));
        }
        if let Some(code) = &failure.code {
            text.push_str(&format!("; errorCode={code}"));
        }
        text.push_str(action);
        text
    };
    let source_tags = store
        .tags(source, "read-source-tags")
        .map_err(|failure| message(&failure, ""))?;
    if source_tags.is_empty() {
        return Ok(());
    }
    let target_tags = match store.tags(target, "read-destination-tags") {
        Ok(tags) => tags,
        Err(failure) if failure.status == Some(405) => {
            tracing::warn!("{}", message(&failure, "; action=skip-tag-preservation"));
            return Ok(());
        }
        Err(failure) => return Err(message(&failure, "")),
    };
    let mut merged = target_tags.clone();
    for (key, value) in source_tags {
        match merged.iter_mut().find(|(known, _)| *known == key) {
            Some(entry) => entry.1 = value,
            None => merged.push((key, value)),
        }
    }
    if merged == target_tags {
        return Ok(());
    }
    if merged.len() > LIMIT {
        let failure = Failure {
            stage: "merge-tag-limit",
            kind: "IllegalStateException",
            status: None,
            code: Some("S3 object tag limit exceeded".to_owned()),
        };
        return Err(message(&failure, ""));
    }
    store
        .put(target, &merged)
        .map_err(|failure| message(&failure, ""))
}

/// An address for messages: S3 objects as they are, others without their details.
fn shown(address: &str) -> String {
    match address.split_once("://") {
        Some(("s3", _)) => address.to_owned(),
        Some((scheme, _)) => format!("{scheme}:[redacted]"),
        None => "file:[redacted]".to_owned(),
    }
}

impl Store {
    /// The `?tagging` address of an `s3://bucket/key` object and its region.
    fn address(&self, object: &str) -> (String, String) {
        let rest = object.strip_prefix("s3://").unwrap_or(object);
        let (bucket, key) = rest.split_once('/').unwrap_or((rest, ""));
        let region = self.access.region.clone().unwrap_or("us-east-1".to_owned());
        let base = match &self.endpoint {
            Some(endpoint) => format!("{}/{bucket}", endpoint.trim_end_matches('/')),
            None => {
                let default = format!("https://{bucket}.s3.{region}.amazonaws.com");
                self.access.endpoint("S3", default)
            }
        };
        (format!("{base}/{key}?tagging"), region)
    }

    fn send(
        &self,
        method: &str,
        object: &str,
        body: Vec<u8>,
        stage: &'static str,
    ) -> Result<String, Failure> {
        let (url, region) = self.address(object);
        let mut headers = Vec::new();
        if !body.is_empty() {
            let digest = base64::engine::general_purpose::STANDARD.encode(Md5::digest(&body));
            headers.push(("content-md5".to_owned(), digest));
            headers.push(("content-type".to_owned(), "application/xml".to_owned()));
        }
        let request = aws::Request {
            service: "s3".to_owned(),
            region,
            method: method.to_owned(),
            url,
            headers,
            body,
        };
        let answer = aws::send(request, &self.access.credentials).map_err(|_| Failure {
            stage,
            kind: "SdkClientException",
            status: None,
            code: None,
        })?;
        let text = String::from_utf8_lossy(&answer.body).into_owned();
        if answer.status >= 300 {
            return Err(Failure {
                stage,
                kind: "S3Exception",
                status: Some(answer.status),
                code: element(&text, "Code"),
            });
        }
        Ok(text)
    }

    fn tags(&self, object: &str, stage: &'static str) -> Result<Vec<(String, String)>, Failure> {
        let text = self.send("GET", object, Vec::new(), stage)?;
        let mut tags = Vec::new();
        for part in text.split("<Tag>").skip(1) {
            let tag = part.split("</Tag>").next().unwrap_or_default();
            if let (Some(key), Some(value)) = (element(tag, "Key"), element(tag, "Value")) {
                tags.push((key, value));
            }
        }
        Ok(tags)
    }

    fn put(&self, object: &str, tags: &[(String, String)]) -> Result<(), Failure> {
        let set: String = (tags.iter())
            .map(|(key, value)| {
                format!(
                    "<Tag><Key>{}</Key><Value>{}</Value></Tag>",
                    escape(key),
                    escape(value)
                )
            })
            .collect();
        let body = format!(
            "<Tagging xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><TagSet>{set}</TagSet></Tagging>"
        );
        self.send("PUT", object, body.into_bytes(), "write-destination-tags")
            .map(drop)
    }
}

/// The text of the first element of this name.
fn element(text: &str, name: &str) -> Option<String> {
    let start = text.find(&format!("<{name}>"))? + name.len() + 2;
    let end = text[start..].find(&format!("</{name}>"))? + start;
    let raw = &text[start..end];
    Some(
        raw.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
