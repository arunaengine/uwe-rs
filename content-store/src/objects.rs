//! The objects of a store as a whole, for clean-up: listing them with their sizes and times,
//! and removing them. Also the mark of the last use of a cache entry.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::ErrorKind as IoErrorKind;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use bytes::Bytes;
use opendal::ErrorKind;
use task_model::Digest;

use crate::{Store, StoreError, layout};

/// An object of a store: a file of a store on this machine, or an object of an object store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Object {
    /// Its location in the store, like `cas/sha256/ab/...`.
    pub location: String,
    pub size: u64,
    /// When it was last written, when the store tells.
    pub modified: Option<SystemTime>,
    /// For a store on this machine, the file on disk. A task output and its content in `cas/`
    /// are two names of one file, which takes its space once.
    pub file: Option<FileIdentity>,
    /// An empty folder of a store on this machine, like an empty directory of a task output.
    pub folder: bool,
}

/// A file on disk: its device and inode, and how many names it has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileIdentity {
    pub device: u64,
    pub inode: u64,
    pub names: u64,
}

impl Store {
    /// Every object whose location starts with `prefix`, like `runs/`, in no particular order.
    pub async fn objects(&self, prefix: &str) -> Result<Vec<Object>, StoreError> {
        match &self.root {
            Some(root) => {
                let root = root.clone();
                let prefix = prefix.to_owned();
                tokio::task::spawn_blocking(move || walk(&root, &prefix))
                    .await
                    .map_err(|error| std::io::Error::other(error.to_string()))?
            }
            None => {
                let mut objects = Vec::new();
                let entries = self.operator.list_with(prefix).recursive(true).await?;
                for entry in entries {
                    let metadata = entry.metadata();
                    if metadata.is_dir() {
                        continue;
                    }
                    objects.push(Object {
                        location: entry.path().to_owned(),
                        size: metadata.content_length(),
                        modified: metadata.last_modified().map(SystemTime::from),
                        file: None,
                        folder: false,
                    });
                }
                Ok(objects)
            }
        }
    }

    /// Removes an object; one that is gone already is no error. In a store on this machine, the
    /// folders that it leaves empty go too, up to the top folder of its kind, like `runs/`.
    pub async fn remove(&self, location: &str) -> Result<(), StoreError> {
        let Some(path) = self.local_path(location) else {
            return Ok(self.operator.delete(location).await?);
        };
        let removed = match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) if metadata.is_dir() => tokio::fs::remove_dir(&path).await,
            Ok(_) => tokio::fs::remove_file(&path).await,
            Err(error) => Err(error),
        };
        match removed {
            Ok(()) => {}
            Err(error) if error.kind() == IoErrorKind::NotFound => return Ok(()),
            // A folder that is not empty keeps what is still in it.
            Err(error) if error.kind() == IoErrorKind::DirectoryNotEmpty => return Ok(()),
            Err(error) => return Err(StoreError::File { path, error }),
        }
        let top =
            (self.root.as_ref()).map(|root| root.join(location.split('/').next().unwrap_or("")));
        let mut folder = path.parent();
        while let Some(current) = folder {
            if Some(current) == top.as_deref() || tokio::fs::remove_dir(current).await.is_err() {
                break;
            }
            folder = current.parent();
        }
        Ok(())
    }

    /// Whether the store has an object, or a folder of a store on this machine. A store in
    /// layers has what its folder or the shared store behind it has.
    pub async fn exists(&self, location: &str) -> Result<bool, StoreError> {
        let here = match self.local_path(location) {
            Some(path) => tokio::fs::try_exists(&path)
                .await
                .map_err(|error| StoreError::File { path, error })?,
            None => self.operator.exists(location).await?,
        };
        Ok(here || self.shared_contains(location).await?)
    }

    /// Whether the store has the directory manifest of a digest.
    pub async fn has_tree(&self, digest: &Digest) -> Result<bool, StoreError> {
        self.exists(&layout::tree(digest)).await
    }

    /// The location in the store of a path or address that [`Store::url`] gave, like
    /// `runs/...` for `/home/me/.cache/iunctus/store/runs/...`; `None` for one outside it.
    pub fn location_of(&self, url: &str) -> Option<String> {
        if let Some(shared) = &self.shared
            && let Some(location) = shared.location_of(url)
        {
            return Some(location);
        }
        let inside = match (&self.root, &self.base) {
            (Some(root), _) => Path::new(url)
                .strip_prefix(root)
                .ok()?
                .to_str()
                .map(str::to_owned),
            (None, Some(base)) => url.strip_prefix(&format!("{base}/")).map(str::to_owned),
            (None, None) => Some(url.to_owned()),
        };
        inside.filter(|location| !location.is_empty())
    }

    /// Notes that a run used the cache entry of a key now, so that clean-up by the time of the
    /// last use keeps it.
    pub async fn mark_used(
        &self,
        namespace: &str,
        key: &Digest,
        run: &str,
    ) -> Result<(), StoreError> {
        let location = layout::used(namespace, key)?;
        self.replace(&location, run.as_bytes()).await?;
        // The shared store keeps its own marks, for its own clean-up.
        if let Some(shared) = &self.shared {
            shared.replace(&location, run.as_bytes()).await?;
        }
        Ok(())
    }

    /// Marks a tool whose task gave other bytes from the same inputs, so that no run shares
    /// its results any more; `note` says what was found. A later mark replaces it.
    pub async fn mark_unreproducible(&self, tool: &Digest, note: Bytes) -> Result<(), StoreError> {
        let location = layout::unreproducible(tool);
        self.operator.write(&location, note.clone()).await?;
        if let Some(shared) = &self.shared {
            shared.operator.write(&location, note).await?;
        }
        Ok(())
    }

    /// The note of a tool that gave other bytes from the same inputs; `None` for a tool that
    /// is not marked.
    pub async fn unreproducible(&self, tool: &Digest) -> Result<Option<Bytes>, StoreError> {
        self.read(&layout::unreproducible(tool)).await
    }

    /// Evicts the cache entry and its last-use mark from this layer only. Shared stale
    /// entries are repaired through [`Store::invalidate_entry`].
    pub async fn remove_entry(&self, namespace: &str, key: &Digest) -> Result<(), StoreError> {
        let _guard = self.entry_guard(&layout::entry(namespace, key)?).await?;
        for location in [
            layout::entry(namespace, key)?,
            layout::used(namespace, key)?,
        ] {
            match self.operator.delete(&location).await {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}

/// The files and empty folders below `root/prefix`, by their locations in the store.
fn walk(root: &Path, prefix: &str) -> Result<Vec<Object>, StoreError> {
    use std::os::unix::fs::MetadataExt;
    let mut objects = Vec::new();
    // The prefix may end inside a name, like `ac/v1/default/ab`; the walk starts at its folder.
    let (folder, start) = match prefix.rsplit_once('/') {
        Some((folder, start)) => (folder.to_owned(), start.to_owned()),
        None => (String::new(), prefix.to_owned()),
    };
    let mut pending: Vec<(PathBuf, String)> = vec![(root.join(&folder), folder)];
    let mut first = true;
    while let Some((directory, location)) = pending.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == IoErrorKind::NotFound => continue,
            Err(error) => {
                return Err(StoreError::File {
                    path: directory,
                    error,
                });
            }
        };
        let mut empty = true;
        for entry in entries {
            let entry = entry?;
            empty = false;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if first && !name.starts_with(&start) {
                continue;
            }
            let inside = match location.is_empty() {
                true => name,
                false => format!("{location}/{name}"),
            };
            let metadata = std::fs::symlink_metadata(entry.path())?;
            if metadata.is_dir() {
                pending.push((entry.path(), inside));
                continue;
            }
            objects.push(Object {
                location: inside,
                size: metadata.len(),
                modified: metadata.modified().ok(),
                file: Some(FileIdentity {
                    device: metadata.dev(),
                    inode: metadata.ino(),
                    names: metadata.nlink(),
                }),
                folder: false,
            });
        }
        if empty && !first {
            let modified = std::fs::metadata(&directory).and_then(|metadata| metadata.modified());
            objects.push(Object {
                location,
                size: 0,
                modified: modified.ok(),
                file: None,
                folder: true,
            });
        }
        first = false;
    }
    Ok(objects)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lists_removes_objects() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let store = Store::local(root.path()).expect("a store");
        let digest = store.put_content("data".into()).await.expect("stores");
        let run = root.path().join("runs/r1/P-1/1/work");
        std::fs::create_dir_all(run.join("db/empty")).expect("makes folders");
        std::fs::write(run.join("out.txt"), "data").expect("writes");
        let mut runs = store.objects("runs/").await.expect("lists");
        runs.sort_by(|a, b| a.location.cmp(&b.location));
        let locations: Vec<(&str, bool)> = (runs.iter())
            .map(|object| (object.location.as_str(), object.folder))
            .collect();
        assert_eq!(
            locations,
            [
                ("runs/r1/P-1/1/work/db/empty", true),
                ("runs/r1/P-1/1/work/out.txt", false)
            ]
        );
        let content = store.objects("cas/").await.expect("lists");
        assert_eq!(content[0].location, layout::content(&digest));
        assert_eq!(content[0].size, 4);
        // A prefix may end inside a name.
        let prefix = &layout::content(&digest)[..12];
        assert_eq!(store.objects(prefix).await.expect("lists").len(), 1);
        for object in &runs {
            store.remove(&object.location).await.expect("removes");
        }
        // The folders that became empty went too, but not the top folder.
        assert!(!root.path().join("runs/r1").exists());
        assert!(root.path().join("runs").exists());
        let url = store.url("runs/r1/x");
        assert_eq!(store.location_of(&url).as_deref(), Some("runs/r1/x"));
        assert_eq!(store.location_of("/elsewhere/x"), None);
    }

    #[tokio::test]
    async fn marks_entry_use() {
        let store = Store::memory().expect("a store");
        let key = Digest::sha256(b"key");
        store
            .put_entry("", &key, "{}".into())
            .await
            .expect("stores");
        store.mark_used("", &key, "run2").await.expect("marks");
        let objects = store.objects("ac/").await.expect("lists");
        assert_eq!(objects.len(), 2);
        store.remove_entry("", &key).await.expect("removes");
        assert!(store.objects("ac/").await.expect("lists").is_empty());
    }
}
