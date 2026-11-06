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
