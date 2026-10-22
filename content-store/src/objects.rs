//! The objects of a store as a whole, for clean-up: listing them with their sizes and times,
//! and removing them. Also the mark of the last use of a cache entry.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::ErrorKind as IoErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use futures_util::TryStreamExt;
use opendal::ErrorKind;
use task_model::Digest;

use crate::{Store, StoreError, layout};

/// An object of a store: a file of a store on this machine, or an object of an object store.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Object {
    /// Its location in the store, like `cas/sha256/ab/...`.
    pub location: String,
    pub size: u64,
    /// When it was last written, when the store tells.
    #[serde(serialize_with = "encode_time", deserialize_with = "decode_time")]
    pub modified: Option<SystemTime>,
    /// For a store on this machine, the file on disk. A task output and its content in `cas/`
    /// are two names of one file, which takes its space once.
    pub file: Option<FileIdentity>,
    /// An empty folder of a store on this machine, like an empty directory of a task output.
    pub folder: bool,
}

/// An object on disk: its device and inode, and how many names it has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct FileIdentity {
    pub device: u64,
    pub inode: u64,
    pub names: u64,
}

/// Traverses inventory without retaining all objects or recursive provider pages.
pub struct Inventory {
    cursor: Cursor,
}

enum Cursor {
    Local {
        directories: Vec<Directory>,
        start: String,
    },
    Remote(opendal::Lister),
}

struct Directory {
    entries: tokio::fs::ReadDir,
    path: PathBuf,
    location: String,
    first: bool,
    empty: bool,
}

impl Inventory {
    pub async fn next(&mut self) -> Result<Option<Object>, StoreError> {
        use std::os::unix::fs::MetadataExt;
        match &mut self.cursor {
            Cursor::Remote(entries) => {
                while let Some(entry) = entries.try_next().await? {
                    let metadata = entry.metadata();
                    if !metadata.is_dir() {
                        return Ok(Some(Object {
                            location: entry.path().to_owned(),
                            size: metadata.content_length(),
                            modified: metadata.last_modified().map(SystemTime::from),
                            file: None,
                            folder: false,
                        }));
                    }
                }
            }
            Cursor::Local { directories, start } => {
                while let Some(directory) = directories.last_mut() {
                    let Some(entry) = directory.entries.next_entry().await? else {
                        let directory = directories.pop().expect("an active directory");
                        if directory.empty && !directory.first {
                            let metadata = tokio::fs::symlink_metadata(directory.path).await?;
                            return Ok(Some(Object {
                                location: directory.location,
                                size: 0,
                                modified: metadata.modified().ok(),
                                file: Some(FileIdentity {
                                    device: metadata.dev(),
                                    inode: metadata.ino(),
                                    names: metadata.nlink(),
                                }),
                                folder: true,
                            }));
                        }
                        continue;
                    };
                    directory.empty = false;
                    let Ok(name) = entry.file_name().into_string() else {
                        continue;
                    };
                    if directory.first && !name.starts_with(start.as_str()) {
                        continue;
                    }
                    let location = if directory.location.is_empty() {
                        name
                    } else {
                        format!("{}/{name}", directory.location)
                    };
                    let metadata = tokio::fs::symlink_metadata(entry.path()).await?;
                    if metadata.is_dir() {
                        directories.push(Directory {
                            entries: tokio::fs::read_dir(entry.path()).await?,
                            path: entry.path(),
                            location,
                            first: false,
                            empty: true,
                        });
                    } else {
                        return Ok(Some(Object {
                            location,
                            size: metadata.len(),
                            modified: metadata.modified().ok(),
                            file: Some(FileIdentity {
                                device: metadata.dev(),
                                inode: metadata.ino(),
                                names: metadata.nlink(),
                            }),
                            folder: false,
                        }));
                    }
                }
            }
        }
        Ok(None)
    }
}

impl Store {
    pub async fn inventory(&self, prefix: &str) -> Result<Inventory, StoreError> {
        let cursor = if let Some(root) = &self.root {
            let (folder, start) = prefix.rsplit_once('/').unwrap_or(("", prefix));
            let path = root.join(folder);
            let directories = match tokio::fs::read_dir(&path).await {
                Ok(entries) => vec![Directory {
                    entries,
                    path,
                    location: folder.to_owned(),
                    first: true,
                    empty: true,
                }],
                Err(error) if error.kind() == IoErrorKind::NotFound => Vec::new(),
                Err(error) => return Err(StoreError::File { path, error }),
            };
            Cursor::Local {
                directories,
                start: start.to_owned(),
            }
        } else {
            Cursor::Remote(self.operator.lister_with(prefix).recursive(true).await?)
        };
        Ok(Inventory { cursor })
    }

    /// Every object whose location starts with `prefix`, like `runs/`, in no particular order.
    pub async fn objects(&self, prefix: &str) -> Result<Vec<Object>, StoreError> {
        let mut entries = self.inventory(prefix).await?;
        let mut objects = Vec::new();
        while let Some(object) = entries.next().await? {
            objects.push(object);
        }
        Ok(objects)
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

    /// Removes only the inventoried local inode, under the caller's maintenance authority.
    pub async fn remove_observed(&self, object: &Object) -> Result<bool, StoreError> {
        if !self.matches_object(object).await? {
            return Ok(false);
        }
        self.remove(&object.location).await?;
        Ok(true)
    }

    async fn matches_object(&self, object: &Object) -> Result<bool, StoreError> {
        use std::os::unix::fs::MetadataExt;
        let Some(path) = self.local_path(&object.location) else {
            return Err(std::io::Error::new(
                IoErrorKind::Unsupported,
                "observed cleanup requires local maintenance authority",
            )
            .into());
        };
        let metadata = match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == IoErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(StoreError::File { path, error }),
        };
        Ok(object
            .file
            .is_some_and(|file| (file.device, file.inode) == (metadata.dev(), metadata.ino()))
            && object.folder == metadata.is_dir()
            && (object.folder || object.size == metadata.len())
            && object.modified == metadata.modified().ok())
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
    /// `runs/...` for `/home/me/.cache/uwe/store/runs/...`; `None` for one outside it.
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

    /// Keeps replacement entries and use marks; observations contain at most those two files.
    pub async fn remove_entry_observed(
        &self,
        namespace: &str,
        key: &Digest,
        objects: &[Object],
    ) -> Result<bool, StoreError> {
        let entry = layout::entry(namespace, key)?;
        let used = layout::used(namespace, key)?;
        if objects.len() > 2
            || objects
                .iter()
                .any(|object| object.location != entry && object.location != used)
        {
            return Err(StoreError::Location(
                "cleanup observations belong to another entry".into(),
            ));
        }
        let _guard = self.entry_guard(&entry).await?;
        for location in [entry, used] {
            match objects.iter().find(|object| object.location == location) {
                Some(object) if self.matches_object(object).await? => {}
                Some(_) => return Ok(false),
                None if self.operator.exists(&location).await? => return Ok(false),
                None => {}
            }
        }
        for object in objects {
            self.remove(&object.location).await?;
        }
        Ok(!objects.is_empty())
    }
}

fn encode_time<S: serde::Serializer>(
    value: &Option<SystemTime>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let offset = value.map(|time| match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(duration) => (false, duration),
        Err(error) => (true, error.duration()),
    });
    serde::Serialize::serialize(&offset, serializer)
}

fn decode_time<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<SystemTime>, D::Error> {
    let offset: Option<(bool, Duration)> = serde::Deserialize::deserialize(deserializer)?;
    offset
        .map(|(before, duration)| {
            if before {
                SystemTime::UNIX_EPOCH.checked_sub(duration)
            } else {
                SystemTime::UNIX_EPOCH.checked_add(duration)
            }
            .ok_or_else(|| serde::de::Error::custom("object time is out of range"))
        })
        .transpose()
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

    #[tokio::test]
    async fn retains_replacement_inode() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::local(root.path()).unwrap();
        let digest = store.put_content("data".into()).await.unwrap();
        let object = store.objects("cas/").await.unwrap().remove(0);
        let replacement = root.path().join("replacement");
        std::fs::write(&replacement, b"data").unwrap();
        std::fs::File::open(&replacement)
            .unwrap()
            .set_modified(object.modified.unwrap())
            .unwrap();
        std::fs::rename(&replacement, root.path().join(&object.location)).unwrap();
        assert!(!store.remove_observed(&object).await.unwrap());
        assert!(store.has_content(&digest).await.unwrap());
        let current = store.objects("cas/").await.unwrap().remove(0);
        assert!(store.remove_observed(&current).await.unwrap());
        assert!(!store.has_content(&digest).await.unwrap());
    }

    #[tokio::test]
    async fn retains_replaced_entries() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::local(root.path()).unwrap();
        let key = Digest::sha256(b"key");
        store.put_entry("", &key, "old".into()).await.unwrap();
        let first = store.objects("ac/").await.unwrap();
        store.mark_used("", &key, "reader").await.unwrap();
        assert!(!store.remove_entry_observed("", &key, &first).await.unwrap());
        let second = store.objects("ac/").await.unwrap();
        store
            .replace(&layout::entry("", &key).unwrap(), b"new")
            .await
            .unwrap();
        assert!(
            !store
                .remove_entry_observed("", &key, &second)
                .await
                .unwrap()
        );
        assert_eq!(
            store.get_entry("", &key).await.unwrap().unwrap(),
            b"new".as_slice()
        );
        assert_eq!(store.objects("ac/").await.unwrap().len(), 2);
        let current = store.objects("ac/").await.unwrap();
        assert!(
            store
                .remove_entry_observed("", &key, &current)
                .await
                .unwrap()
        );
        assert!(store.objects("ac/").await.unwrap().is_empty());
    }
}
