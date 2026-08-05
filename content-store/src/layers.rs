//! A store in layers: a store in a folder of this machine in front of a shared store, like one
//! on S3. Reads look in the folder first and take what it lacks from the shared store, keeping
//! a copy in the folder; new objects go to both. Leases live in the shared store, so that runs
//! on several machines see each other. Clean-up works on the folder only; the shared store is
//! cleaned on its own.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use opendal::{ErrorKind, Operator};
use tokio::io::AsyncWriteExt;

use crate::store::PART_SIZE;
use crate::{Store, StoreError, local};

impl Store {
    /// A store whose objects live in the store `local`, in a folder of this machine, and in
    /// the store `shared` behind it.
    pub fn layered(mut local: Store, shared: Store) -> Result<Store, StoreError> {
        if local.root.is_none() {
            return Err(StoreError::Location(
                "the front layer of a store must be a folder of this machine".into(),
            ));
        }
        if shared.shared.is_some() {
            return Err(StoreError::Location(
                "the shared layer of a store cannot have layers of its own".into(),
            ));
        }
        local.shared = Some(Arc::new(shared));
        Ok(local)
    }

    /// The shared store behind a store in layers.
    pub fn shared(&self) -> Option<&Store> {
        self.shared.as_deref()
    }

    /// The bytes of an object: from this store, else from the shared store behind it, which
    /// this store then keeps a copy of. `None` when neither has it.
    pub(crate) async fn read(&self, location: &str) -> Result<Option<Bytes>, StoreError> {
        match self.operator.read(location).await {
            Ok(buffer) => return Ok(Some(buffer.to_bytes())),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let Some(shared) = &self.shared else {
            return Ok(None);
        };
        let bytes = match shared.operator.read(location).await {
            Ok(buffer) => buffer.to_bytes(),
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if let Some(target) = self.local_path(location) {
            local::write_absent(&target, &bytes)
                .await
                .map_err(at(&target))?;
        }
        Ok(Some(bytes))
    }

    /// The object at `location` as a file or folder of this machine: the one of a store in a
    /// folder; for a store in layers, the one of its folder, taken from the shared store first
    /// when the folder lacks it, a folder with all its files. `None` for a store elsewhere.
    pub async fn local_file(&self, location: &str) -> Result<Option<PathBuf>, StoreError> {
        let Some(path) = self.local_path(location) else {
            return Ok(None);
        };
        let Some(shared) = &self.shared else {
            return Ok(Some(path));
        };
        if tokio::fs::try_exists(&path).await.map_err(at(&path))? {
            return Ok(Some(path));
        }
        match shared.local_path(location) {
            Some(source) if source.is_dir() => copy_folder(&source, &path).await?,
            Some(source) => local::copy_absent(&source, &path)
                .await
                .map_err(at(&path))?,
            None if shared.operator.exists(location).await? => {
                download(&shared.operator, location, &path).await?;
            }
            None => {
                let folder = format!("{}/", location.trim_end_matches('/'));
                let entries = shared.operator.list_with(&folder).recursive(true).await?;
                for entry in entries.iter().filter(|entry| !entry.metadata().is_dir()) {
                    let inside = entry.path().strip_prefix(&folder).unwrap_or(entry.path());
                    download(&shared.operator, entry.path(), &path.join(inside)).await?;
                }
            }
        }
        Ok(Some(path))
    }

    /// Whether the shared store behind a store in layers has an object or a folder at
    /// `location`, like a directory output of a task, whose files an object store keeps below
    /// its name.
    pub(crate) async fn shared_contains(&self, location: &str) -> Result<bool, StoreError> {
        let Some(shared) = &self.shared else {
            return Ok(false);
        };
        if let Some(path) = shared.local_path(location) {
            return tokio::fs::try_exists(&path).await.map_err(at(&path));
        }
        if shared.operator.exists(location).await? {
            return Ok(true);
        }
        let folder = format!("{}/", location.trim_end_matches('/'));
        let entries = shared.operator.list_with(&folder).limit(1).await?;
        Ok(!entries.is_empty())
    }
}

/// Downloads an object in parts to a file of this machine, under a temporary name first, so
/// that no reader sees half a file.
async fn download(operator: &Operator, location: &str, target: &Path) -> Result<(), StoreError> {
    if let Some(folder) = target.parent() {
        tokio::fs::create_dir_all(folder)
            .await
            .map_err(at(folder))?;
    }
    let size = operator.stat(location).await?.content_length();
    let reader = operator.reader(location).await?;
    let part = PathBuf::from(format!("{}.part-{}", target.display(), std::process::id()));
    let mut file = tokio::fs::File::create(&part).await.map_err(at(&part))?;
    let mut offset = 0;
    while offset < size {
        let end = (offset + PART_SIZE as u64).min(size);
        let buffer = reader.read(offset..end).await?;
        file.write_all(&buffer.to_bytes())
            .await
            .map_err(at(&part))?;
        offset = end;
    }
    file.flush().await.map_err(at(&part))?;
    drop(file);
    tokio::fs::rename(&part, target).await.map_err(at(target))
}

/// Copies the files of a folder of this machine into another, where they are not yet.
async fn copy_folder(source: &Path, target: &Path) -> Result<(), StoreError> {
    let mut pending = vec![(source.to_owned(), target.to_owned())];
    while let Some((from, to)) = pending.pop() {
        tokio::fs::create_dir_all(&to).await.map_err(at(&to))?;
        let mut entries = tokio::fs::read_dir(&from).await.map_err(at(&from))?;
        while let Some(entry) = entries.next_entry().await.map_err(at(&from))? {
            let inside = to.join(entry.file_name());
            match entry.file_type().await.map_err(at(&inside))?.is_dir() {
                true => pending.push((entry.path(), inside)),
                false => local::copy_absent(&entry.path(), &inside)
                    .await
                    .map_err(at(&inside))?,
            }
        }
    }
    Ok(())
}

/// An error of a file of this machine, with its path.
fn at(path: &Path) -> impl FnOnce(std::io::Error) -> StoreError + '_ {
    move |error| StoreError::File {
        path: path.to_owned(),
        error,
    }
}
