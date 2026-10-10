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

/// Manifest association for a logical directory location, outside user output paths.
pub fn directory(location: &str) -> String {
    format!(
        "directories/v1/{}.json",
        Digest::sha256(location.as_bytes()).hex()
    )
}

/// A complete local directory copy, published separately from child object locations.
pub fn directory_snapshot(digest: &Digest) -> String {
    format!("directory-snapshots/v1/{}", digest.hex())
}

/// The mark of a tool that gave other bytes from the same inputs when a task of it ran again:
/// `tools/<digest hex>.json`, where the digest names the tool (its image and step).
pub fn unreproducible(tool: &Digest) -> String {
    format!("tools/{}.json", tool.hex())
}

/// The folder of leases (see [`crate::Lease`]).
pub const LEASES: &str = "locks";

/// Cache entries: `ac/v1/<namespace>/<cache key hex>.json`.
pub fn entry(namespace: &str, key: &Digest) -> Result<String, StoreError> {
    Ok(format!("{}.json", entry_base(namespace, key)?))
}

/// The mark of the last use of a cache entry: `ac/v1/<namespace>/<cache key hex>.used`. Its
/// time is when a run last took the entry.
pub fn used(namespace: &str, key: &Digest) -> Result<String, StoreError> {
    Ok(format!("{}.used", entry_base(namespace, key)?))
}

/// The namespace and key of a cache entry or its mark, from its location; `None` for another
/// location. The default namespace is the empty one.
pub fn entry_key(location: &str) -> Option<(String, Digest)> {
    let rest = location.strip_prefix(&format!("ac/{ENTRY_VERSION}/"))?;
    let (namespace, name) = rest.split_once('/')?;
    let hex = name
        .strip_suffix(".json")
        .or_else(|| name.strip_suffix(".used"))?;
    let key = format!("sha256:{hex}").parse().ok()?;
    let namespace = match namespace {
        DEFAULT_NAMESPACE => String::new(),
        other => other.to_owned(),
    };
    Some((namespace, key))
}

/// A lease: `locks/<name>`, where each part of the name is a plain name, like
/// `locks/tasks/default/<cache key hex>`.
pub fn lease(name: &str) -> Result<String, StoreError> {
    let parts: Vec<&str> = name.split('/').collect();
    if parts.iter().any(|part| !plain(part)) {
        return Err(StoreError::Location(format!("invalid lease name `{name}`")));
    }
    Ok(format!("{LEASES}/{name}"))
}

fn entry_base(namespace: &str, key: &Digest) -> Result<String, StoreError> {
    let namespace = if namespace.is_empty() {
        DEFAULT_NAMESPACE
    } else {
        namespace
    };
    if !plain(namespace) {
        let message = format!("invalid cache namespace `{namespace}`");
        return Err(StoreError::Location(message));
    }
    Ok(format!("ac/{ENTRY_VERSION}/{namespace}/{}", key.hex()))
}

/// A name of letters, digits, `.`, `_` and `-` that does not start with `.`.
fn plain(name: &str) -> bool {
    let valid = name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    valid && !name.is_empty() && !name.starts_with('.')
}

/// The namespace of cache entries, as the tasks of the default namespace name it.
pub fn namespace_name(namespace: &str) -> &str {
    if namespace.is_empty() {
        DEFAULT_NAMESPACE
    } else {
        namespace
    }
}

/// Keeps task output names under the run, task and attempt work or absolute-root subtree.
pub fn output(run: &str, task: &str, attempt: u32, path: &str) -> Result<String, StoreError> {
    let unsafe_part =
        |part: &str| part.is_empty() || part == "." || part == ".." || part.contains('/');
    if path == "." && !unsafe_part(run) && !unsafe_part(task) {
        return Ok(format!("runs/{run}/{task}/{attempt}/work"));
    }
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
