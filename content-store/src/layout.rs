//! Where each kind of object lives inside a store; the same layout serves directories and S3.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use task_model::Digest;

use crate::StoreError;

/// Version of the cache entry layout, so a new entry format can live next to the old one.
const ENTRY_VERSION: &str = "v1";

/// The namespace segment used for the default, empty cache namespace.
const DEFAULT_NAMESPACE: &str = "default";

/// File contents: `cas/sha256/<2 hex>/<62 hex>`. Two levels keep directories small.
pub fn content(digest: &Digest) -> String {
    sharded("cas", digest)
}

/// Directory manifests: `trees/sha256/<2 hex>/<62 hex>`.
pub fn tree(digest: &Digest) -> String {
    sharded("trees", digest)
}

/// Cache entries: `ac/v1/<namespace>/<cache key hex>.json`.
pub fn entry(namespace: &str, key: &Digest) -> Result<String, StoreError> {
    let namespace = if namespace.is_empty() {
        DEFAULT_NAMESPACE
    } else {
        namespace
    };
    let valid = namespace
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if !valid || namespace.starts_with('.') {
        let message = format!("invalid cache namespace `{namespace}`");
        return Err(StoreError::Location(message));
    }
    Ok(format!("ac/{ENTRY_VERSION}/{namespace}/{}.json", key.hex()))
}

/// Task outputs under their original names: `runs/<run>/<task>/<attempt>/work/<path>` for a
/// path relative to the work directory, and `runs/<run>/<task>/<attempt>/root/<path>` for an
/// absolute path inside the container.
pub fn output(run: &str, task: &str, attempt: u32, path: &str) -> Result<String, StoreError> {
    let unsafe_part =
        |part: &str| part.is_empty() || part == "." || part == ".." || part.contains('/');
    let (base, relative) = match path.strip_prefix('/') {
        Some(inside) => ("root", inside),
        None => ("work", path),
    };
    let escapes = relative.split('/').any(unsafe_part);
    if relative.is_empty() || escapes || unsafe_part(run) || unsafe_part(task) {
        let message = format!("invalid output path `{run}/{task}/{attempt}/{path}`");
        return Err(StoreError::Location(message));
    }
    Ok(format!("runs/{run}/{task}/{attempt}/{base}/{relative}"))
}

fn sharded(root: &str, digest: &Digest) -> String {
    let hex = digest.hex();
    format!("{root}/sha256/{}/{}", &hex[..2], &hex[2..])
}
