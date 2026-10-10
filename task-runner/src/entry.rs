//! Validates and reads cached task outputs, stream content and execution metadata.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::PermissionsExt;

use content_store::{EntrySnapshot, Store, StoreError};
use serde::{Deserialize, Serialize};
use task_model::{Digest, TreeEntry};

use crate::{ContainerResult, RunnerError, StoredFile, TaskResult};

/// A cache entry, as `ac/v1/<namespace>/<key>.json` holds it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheEntry {
    /// The run that made the result.
    pub run: String,
    /// The task in that run, as the store names its folder, like `FASTQC-12`.
    pub task: String,
    /// The raw exit code; older entries contain only zero-code results.
    #[serde(default)]
    pub exit_code: i32,
    pub files: Vec<StoredFile>,
    pub stdout: Option<Digest>,
    pub stderr: Option<Digest>,
    /// How long the task ran, in milliseconds; entries of earlier versions do not tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milliseconds: Option<u64>,
    /// How the containers of a task that runs several ended, the main one first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub containers: Vec<ContainerResult>,
}

impl CacheEntry {
    pub fn parse(key: &Digest, bytes: &[u8]) -> Result<CacheEntry, RunnerError> {
        serde_json::from_slice(bytes).map_err(|error| RunnerError::Entry {
            key: key.clone(),
            message: error.to_string(),
        })
    }

    pub fn to_bytes(&self, key: &Digest) -> Result<Vec<u8>, RunnerError> {
        serde_json::to_vec(self).map_err(|error| RunnerError::Entry {
            key: key.clone(),
            message: error.to_string(),
        })
    }

    /// The result that the entry gives a request.
    pub fn result(self) -> TaskResult {
        TaskResult {
            exit_code: Some(self.exit_code),
            files: self.files,
            stdout: self.stdout,
            stderr: self.stderr,
            reused: true,
            usage: None,
            started: None,
            ended: None,
            check: None,
            containers: self.containers,
        }
    }

    /// The digests of the captured streams: of the task, and of its other containers.
    pub fn logs(&self) -> impl Iterator<Item = &Digest> {
        let containers = self.containers.iter();
        (self.stdout.iter().chain(&self.stderr)).chain(
            containers.flat_map(|container| container.stdout.iter().chain(&container.stderr)),
        )
    }

    /// Whether the store still has every file of the entry: each output, the manifest of each
    /// directory, and the captured streams.
    pub async fn complete(&self, store: &Store) -> Result<bool, RunnerError> {
        let verified = tempfile::tempdir()?;
        for file in &self.files {
            // In an object store, the location of a directory is only the prefix of its files,
            // no object itself; its manifest stands for it there.
            let checkable = !file.directory || store.local_path(&file.location).is_some();
            let present = (!file.directory || store.has_tree(&file.digest).await?)
                && (!checkable || store.exists(&file.location).await?);
            if !present {
                return Ok(false);
            }
            if file.directory && !directory_complete(store, file).await? {
                return Ok(false);
            }
            if !file.directory {
                let target = verified.path().join("output");
                match store
                    .materialize_object(&file.location, &file.digest, &target)
                    .await
                {
                    Ok(()) => {}
                    Err(error) if damaged(&error) => return Ok(false),
                    Err(error) => return Err(error.into()),
                }
                if tokio::fs::metadata(&target).await?.len() != file.size {
                    return Ok(false);
                }
                match store.materialize_content(&file.digest, &target).await {
                    Ok(()) => {}
                    Err(error) if damaged(&error) => return Ok(false),
                    Err(error) => return Err(error.into()),
                }
            }
        }
        for digest in self.logs() {
            match store
                .materialize_content(digest, &verified.path().join("stream"))
                .await
            {
                Ok(()) => {}
                Err(error) if damaged(&error) => return Ok(false),
                Err(error) => return Err(error.into()),
            }
        }
        Ok(true)
    }
}

fn damaged(error: &StoreError) -> bool {
    match error {
        StoreError::Corrupt { .. } | StoreError::Tree { .. } | StoreError::InvalidTree(_) => true,
        _ => error.missing(),
    }
}

async fn directory_complete(store: &Store, file: &StoredFile) -> Result<bool, RunnerError> {
    let tree = match store.get_tree(&file.digest).await {
        Ok(tree) => tree,
        Err(error) if damaged(&error) => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if tree.digest().ok().as_ref() != Some(&file.digest) || tree.size() != file.size {
        return Ok(false);
    }
    let root = store
        .local_path(&file.location)
        .filter(|path| path.exists())
        .or_else(|| {
            store
                .shared()
                .and_then(|shared| shared.local_path(&file.location))
                .filter(|path| path.exists())
        });
    if let Some(root) = &root
        && !tokio::fs::symlink_metadata(root).await?.is_dir()
    {
        return Ok(false);
    }
    if let Some(root) = &root {
        let mut expected = std::collections::BTreeSet::new();
        let mut parents = std::collections::BTreeSet::new();
        for entry in &tree.entries {
            let mut path: std::path::PathBuf = std::path::Path::new(entry.path())
                .components()
                .filter(|part| *part != std::path::Component::CurDir)
                .collect();
            let member = path.clone();
            while !path.as_os_str().is_empty() {
                expected.insert(path.clone());
                if path != member {
                    parents.insert(path.clone());
                }
                if !path.pop() {
                    break;
                }
            }
        }
        let mut pending = vec![(root.clone(), std::path::PathBuf::new())];
        while let Some((directory, relative)) = pending.pop() {
            let mut entries = tokio::fs::read_dir(directory).await?;
            while let Some(entry) = entries.next_entry().await? {
                let relative = relative.join(entry.file_name());
                if !expected.contains(&relative) {
                    return Ok(false);
                }
                let kind = entry.file_type().await?;
                if parents.contains(&relative) && !kind.is_dir() {
                    return Ok(false);
                }
                if kind.is_dir() {
                    pending.push((entry.path(), relative));
                }
            }
        }
    }
    let verified = tempfile::tempdir()?;
    for entry in tree.entries {
        let relative = entry.path();
        if relative.is_empty()
            || relative.starts_with('/')
            || relative.split('/').any(|part| part == "..")
        {
            return Ok(false);
        }
        let member = root.as_ref().map(|root| root.join(relative));
        match entry {
            TreeEntry::File {
                path,
                digest,
                size,
                executable,
            } => {
                if let Some(member) = member {
                    let metadata = match tokio::fs::symlink_metadata(&member).await {
                        Ok(metadata) => metadata,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            return Ok(false);
                        }
                        Err(error) => return Err(error.into()),
                    };
                    if !metadata.is_file()
                        || metadata.len() != size
                        || (metadata.permissions().mode() & 0o111 != 0) != executable
                    {
                        return Ok(false);
                    }
                }
                let target = verified.path().join("member");
                let location = format!("{}/{path}", file.location.trim_end_matches('/'));
                for content in [false, true] {
                    let result = if content {
                        store.materialize_content(&digest, &target).await
                    } else {
                        store.materialize_object(&location, &digest, &target).await
                    };
                    match result {
                        Ok(()) => {}
                        Err(error) if damaged(&error) => return Ok(false),
                        Err(error) => return Err(error.into()),
                    }
                }
                if tokio::fs::metadata(target).await?.len() != size {
                    return Ok(false);
                }
            }
            TreeEntry::EmptyDirectory { .. } => {
                if let Some(member) = member {
                    match tokio::fs::symlink_metadata(&member).await {
                        Ok(metadata) if metadata.is_dir() => {
                            if tokio::fs::read_dir(member)
                                .await?
                                .next_entry()
                                .await?
                                .is_some()
                            {
                                return Ok(false);
                            }
                        }
                        Ok(_) => return Ok(false),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            return Ok(false);
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
            }
            TreeEntry::Symlink { path, target } => {
                let mut depth = std::path::Path::new(&path)
                    .parent()
                    .map_or(0, |parent| parent.components().count());
                if !std::path::Path::new(&target)
                    .components()
                    .all(|part| match part {
                        std::path::Component::Normal(_) => {
                            depth += 1;
                            true
                        }
                        std::path::Component::ParentDir if depth > 0 => {
                            depth -= 1;
                            true
                        }
                        std::path::Component::CurDir => true,
                        _ => false,
                    })
                {
                    return Ok(false);
                }
                if let Some(member) = member {
                    match tokio::fs::read_link(member).await {
                        Ok(actual) if actual.as_os_str() == std::ffi::OsStr::new(&target) => {}
                        Ok(_) => return Ok(false),
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput
                            ) =>
                        {
                            return Ok(false);
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
            }
        }
    }
    Ok(true)
}

/// What to do with an entry whose files are gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stale {
    /// Remove it, so that the task runs again and stores a new entry.
    Remove,
    /// Leave it, as a lookup without side effects does.
    Keep,
}

/// The stored entry of a key, when it is complete.
pub(crate) async fn stored(
    store: &Store,
    namespace: &str,
    key: &Digest,
    stale: Stale,
) -> Result<Option<CacheEntry>, RunnerError> {
    Ok(observed(store, namespace, key, stale)
        .await?
        .map(|found| found.entry))
}

pub(crate) struct Observed {
    pub entry: CacheEntry,
    pub snapshot: EntrySnapshot,
}

pub(crate) async fn observed(
    store: &Store,
    namespace: &str,
    key: &Digest,
    stale: Stale,
) -> Result<Option<Observed>, RunnerError> {
    let Some(observed) = store.observe_entry(namespace, key).await? else {
        return Ok(None);
    };
    if let Ok(entry) = CacheEntry::parse(key, observed.bytes())
        && entry.complete(store).await?
    {
        return Ok(Some(Observed {
            entry,
            snapshot: observed,
        }));
    }
    if stale == Stale::Remove {
        store.invalidate_entry(namespace, key, &observed).await?;
    }
    Ok(None)
}
