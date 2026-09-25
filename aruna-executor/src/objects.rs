//! Immutable object transfer supplied by the embedding host, without invented API routes.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use executor::ExecutorError;
use serde::{Deserialize, Serialize};

use crate::client::Object;

pub type Transfer<'a, T> = Pin<Box<dyn Future<Output = Result<T, ExecutorError>> + Send + 'a>>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reference {
    pub bucket: String,
    pub key: String,
    pub version_id: String,
}

/// An authenticated host adapter bound to one caller and its allowed group and buckets.
/// Writes return completed immutable versions. Reads must authorize `endpoint_url`, read
/// exactly `version_id`, enforce the byte limit while streaming, and verify object integrity.
/// Credentials must never be forwarded to an endpoint taken only from a job response.
pub trait Objects: Send + Sync + 'static {
    fn write<'a>(&'a self, group: &'a str, key: &'a str, file: &'a Path)
    -> Transfer<'a, Reference>;
    fn read<'a>(&'a self, object: &'a Object, maximum: u64) -> Transfer<'a, Vec<u8>>;
}

/// Maximum encoded input or result bundle size. Bundles are uncompressed tar archives.
pub const MAX_BUNDLE: u64 = 256 * 1024 * 1024;

/// Restores only regular files and directories below the existing wrapper's result roots.
/// Links, devices, parent paths and absolute archive paths are refused before extraction.
pub fn collect(bytes: &[u8], root: &Path) -> Result<executor::Finished, ExecutorError> {
    use std::path::Component;
    if bytes.len() as u64 > MAX_BUNDLE {
        return Err(std::io::Error::other("Aruna result bundle exceeds the size limit").into());
    }
    let mut archive = tar::Archive::new(bytes);
    let mut total = 0u64;
    for (index, entry) in archive.entries()?.enumerate() {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let parts: Vec<_> = path.components().collect();
        total = total.saturating_add(entry.size());
        if index >= 100_000
            || total > MAX_BUNDLE
            || !matches!(parts.first(), Some(Component::Normal(name)) if *name == "control" || *name == "outbox")
            || parts
                .iter()
                .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
            || !(entry.header().entry_type().is_file() || entry.header().entry_type().is_dir())
            || !entry.unpack_in(root)?
        {
            return Err(std::io::Error::other("unsafe or oversized Aruna result bundle").into());
        }
    }
    std::fs::rename(root.join("outbox"), root.join(executor::WORK))?;
    let control = root.join(executor::CONTROL);
    let finished = executor::Finished::from_control(root.to_owned())?;
    if finished.exit_code.is_none()
        || (finished.exit_code == Some(0) && !control.join(executor::wrapper::MANIFEST).is_file())
    {
        return Err(std::io::Error::other("Aruna bundle has no complete wrapper records").into());
    }
    for file in &finished.files {
        let metadata = std::fs::metadata(finished.local_path(&file.path))?;
        if metadata.is_dir() != file.directory || !(metadata.is_file() || metadata.is_dir()) {
            return Err(std::io::Error::other("Aruna output disagrees with its manifest").into());
        }
    }
    Ok(finished)
}
