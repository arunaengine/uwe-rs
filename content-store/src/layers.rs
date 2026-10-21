//! Caches shared store reads in a local folder and publishes objects to both layers.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use opendal::{ErrorKind, Operator};
use sha2::{Digest as _, Sha256};
use task_model::{Digest, Tree, TreeEntry};
use tokio::io::AsyncWriteExt;

use crate::store::PART_SIZE;
use crate::{Store, StoreError, hash_file, layout, local};

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
        if self.cache_reads
            && let Some(target) = self.local_path(location)
        {
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
        match tokio::fs::metadata(&path).await {
            Ok(metadata) if metadata.is_file() => return Ok(Some(path)),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(at(&path)(error)),
        }
        if let Some(digest) = self.directory(location).await? {
            return self.hydrate_directory(&digest).await.map(Some);
        }
        match shared.local_path(location) {
            Some(source) if source.is_dir() => {
                return self.hydrate_legacy(location, Some(source)).await.map(Some);
            }
            Some(source) => local::copy_absent(&source, &path)
                .await
                .map_err(at(&path))?,
            None => match shared.operator.stat(location).await {
                Ok(metadata) if metadata.is_file() => {
                    download(&shared.operator, location, &path).await?
                }
                Ok(_) => return self.hydrate_legacy(location, None).await.map(Some),
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    return self.hydrate_legacy(location, None).await.map(Some);
                }
                Err(error) => return Err(error.into()),
            },
        }
        Ok(Some(path))
    }

    async fn hydrate_legacy(
        &self,
        location: &str,
        source: Option<PathBuf>,
    ) -> Result<PathBuf, StoreError> {
        let parent = self
            .local_path("directory-snapshots/legacy")
            .expect("a front layer");
        std::fs::create_dir_all(&parent).map_err(at(&parent))?;
        let mut temporary = tempfile::tempdir_in(&parent).map_err(at(&parent))?;
        let root = temporary.path().join("input");
        std::fs::create_dir(&root).map_err(at(&root))?;
        if let Some(source) = source {
            temporary = copy_folder(source, temporary).await?;
        } else {
            let shared = self.shared.as_ref().expect("a shared layer");
            let folder = format!("{}/", location.trim_end_matches('/'));
            let entries = shared.operator.list_with(&folder).recursive(true).await?;
            if entries.is_empty() && !shared.operator.stat(&folder).await?.is_dir() {
                return Err(opendal::Error::new(ErrorKind::NotFound, location.to_owned()).into());
            }
            for entry in entries {
                let inside = entry
                    .path()
                    .strip_prefix(&folder)
                    .ok_or_else(|| StoreError::Location(entry.path().to_owned()))?;
                if Path::new(inside)
                    .components()
                    .any(|part| !matches!(part, Component::Normal(_)))
                {
                    return Err(StoreError::Location(entry.path().to_owned()));
                }
                let target = root.join(inside);
                if entry.metadata().is_dir() {
                    std::fs::create_dir_all(&target).map_err(at(&target))?;
                } else {
                    download(&shared.operator, entry.path(), &target).await?;
                }
            }
        }
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o755))?;
        let _ = temporary.keep();
        Ok(root)
    }

    async fn hydrate_directory(&self, digest: &Digest) -> Result<PathBuf, StoreError> {
        let tree = self.get_tree(digest).await?;
        validate_tree(&tree)?;
        let target = self
            .local_path(&layout::directory_snapshot(digest))
            .expect("a front layer");
        let input = target.join("input");
        if target.try_exists().map_err(at(&target))? {
            verify_directory(&input, &tree, true).await?;
            return Ok(input);
        }
        let parent = target.parent().expect("a parent");
        std::fs::create_dir_all(parent).map_err(at(parent))?;
        let temporary = tempfile::tempdir_in(parent).map_err(at(parent))?;
        let root = temporary.path().join("input");
        std::fs::create_dir(&root).map_err(at(&root))?;
        let shared = self.shared.as_ref().expect("a shared layer");
        for entry in &tree.entries {
            let inside = root.join(entry.path());
            match entry {
                TreeEntry::File {
                    digest,
                    size,
                    executable,
                    ..
                } => {
                    download_file(
                        &shared.operator,
                        &layout::content(digest),
                        &inside,
                        Some((digest, *size)),
                    )
                    .await?;
                    let mode = if *executable { 0o755 } else { 0o644 };
                    std::fs::set_permissions(&inside, std::fs::Permissions::from_mode(mode))
                        .map_err(at(&inside))?;
                }
                TreeEntry::EmptyDirectory { .. } => {
                    std::fs::create_dir_all(&inside).map_err(at(&inside))?
                }
                TreeEntry::Symlink { .. } => {}
            }
        }
        for entry in &tree.entries {
            if let TreeEntry::Symlink { path, target } = entry {
                let inside = root.join(path);
                std::fs::create_dir_all(inside.parent().expect("a parent")).map_err(at(&inside))?;
                std::os::unix::fs::symlink(target, &inside).map_err(at(&inside))?;
            }
        }
        verify_directory(&root, &tree, false).await?;
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o755))?;
        match std::fs::rename(temporary.path(), &target) {
            Ok(()) => {}
            Err(_) if target.is_dir() => {
                verify_directory(&input, &tree, true).await?;
            }
            Err(error) => return Err(at(&target)(error)),
        }
        Ok(input)
    }

    /// Whether the shared store behind a store in layers has an object or a folder at
    /// `location`, like a directory output of a task, whose files an object store keeps below
    /// its name.
    pub(crate) async fn shared_contains(&self, location: &str) -> Result<bool, StoreError> {
        let Some(shared) = &self.shared else {
            return Ok(false);
        };
        if self.directory(location).await?.is_some() {
            return Ok(true);
        }
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
    download_file(operator, location, target, None).await
}

async fn download_file(
    operator: &Operator,
    location: &str,
    target: &Path,
    expected: Option<(&Digest, u64)>,
) -> Result<(), StoreError> {
    if let Some(folder) = target.parent() {
        std::fs::create_dir_all(folder).map_err(at(folder))?;
    }
    let size = operator.stat(location).await?.content_length();
    if expected.is_some_and(|(_, expected)| expected != size) {
        return Err(StoreError::Location(format!(
            "directory member {location} has an unexpected size"
        )));
    }
    let reader = operator.reader(location).await?;
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let part = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o666))
        .tempfile_in(parent)
        .map_err(at(target))?;
    let mut file = tokio::fs::File::from_std(part.reopen().map_err(at(target))?);
    let mut hasher = Sha256::new();
    let mut offset = 0;
    while offset < size {
        let end = (offset + PART_SIZE as u64).min(size);
        let buffer = reader.read(offset..end).await?.to_bytes();
        if buffer.len() as u64 != end - offset {
            return Err(opendal::Error::new(
                ErrorKind::Unexpected,
                "stored content changed size while being read",
            )
            .into());
        }
        file.write_all(&buffer).await.map_err(at(target))?;
        if expected.is_some() {
            hasher.update(&buffer);
        }
        offset = end;
    }
    file.flush().await.map_err(at(target))?;
    drop(file);
    if let Some((expected, _)) = expected {
        let actual = Digest::from_sha256(&hasher.finalize());
        if &actual != expected {
            return Err(StoreError::Corrupt {
                expected: expected.clone(),
                actual,
            });
        }
    }
    // A copy in the folder is read-only and on disk before it is in place, like other objects.
    let file = part.as_file();
    let mode = file.metadata().map_err(at(target))?.permissions().mode();
    (file.set_permissions(std::fs::Permissions::from_mode(mode & !0o222)))
        .and_then(|()| file.sync_all())
        .map_err(at(target))?;
    part.persist(target)
        .map(|_| ())
        .map_err(|error| at(target)(error.error))
}

fn validate_tree(tree: &Tree) -> Result<(), StoreError> {
    tree.canonical()?;
    let members: BTreeSet<_> = tree.entries.iter().map(TreeEntry::path).collect();
    for entry in &tree.entries {
        if entry.path().is_empty()
            || Path::new(entry.path())
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
            || Path::new(entry.path())
                .ancestors()
                .skip(1)
                .any(|parent| members.contains(parent.to_str().unwrap_or("")))
        {
            return Err(StoreError::Location(format!(
                "invalid directory member {}",
                entry.path()
            )));
        }
        if let TreeEntry::Symlink { path, target } = entry {
            let mut depth = Path::new(path).components().count() - 1;
            for component in Path::new(target).components() {
                match component {
                    Component::Normal(_) => depth += 1,
                    Component::CurDir => {}
                    Component::ParentDir if depth > 0 => depth -= 1,
                    _ => {
                        return Err(StoreError::Location(format!(
                            "directory link {path} leaves its tree"
                        )));
                    }
                }
            }
            if target.is_empty() {
                return Err(StoreError::Location(format!(
                    "directory link {path} has no target"
                )));
            }
        }
    }
    Ok(())
}

async fn verify_directory(root: &Path, tree: &Tree, contents: bool) -> Result<(), StoreError> {
    if !std::fs::symlink_metadata(root).map_err(at(root))?.is_dir() {
        return Err(StoreError::Location(format!(
            "{} is not a directory",
            root.display()
        )));
    }
    let mut members = BTreeSet::new();
    let leaves: BTreeSet<_> = tree
        .entries
        .iter()
        .map(|entry| PathBuf::from(entry.path()))
        .collect();
    for entry in &tree.entries {
        members.extend(
            Path::new(entry.path())
                .ancestors()
                .filter(|path| !path.as_os_str().is_empty())
                .map(Path::to_owned),
        );
    }
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        let directory = root.join(&relative);
        for entry in std::fs::read_dir(&directory).map_err(at(&directory))? {
            let entry = entry.map_err(at(&directory))?;
            let inside = relative.join(entry.file_name());
            if !members.contains(&inside) {
                return Err(StoreError::Location(format!(
                    "unexpected directory member {}",
                    inside.display()
                )));
            }
            let kind = entry.file_type()?;
            if !leaves.contains(&inside) && !kind.is_dir() {
                return Err(StoreError::Location(format!(
                    "{} is not a directory",
                    inside.display()
                )));
            }
            if kind.is_dir() {
                pending.push(inside);
            }
        }
    }
    let canonical = std::fs::canonicalize(root).map_err(at(root))?;
    for entry in &tree.entries {
        let path = root.join(entry.path());
        let metadata = std::fs::symlink_metadata(&path).map_err(at(&path))?;
        let valid = match entry {
            TreeEntry::File {
                digest,
                size,
                executable,
                ..
            } => {
                if !metadata.is_file()
                    || metadata.len() != *size
                    || (metadata.permissions().mode() & 0o111 != 0) != *executable
                {
                    false
                } else {
                    if contents {
                        let (actual, _) = hash_file(&path).await?;
                        if &actual != digest {
                            return Err(StoreError::Corrupt {
                                expected: digest.clone(),
                                actual,
                            });
                        }
                    }
                    true
                }
            }
            TreeEntry::EmptyDirectory { .. } => {
                metadata.is_dir()
                    && std::fs::read_dir(&path)
                        .map_err(at(&path))?
                        .next()
                        .is_none()
            }
            TreeEntry::Symlink { target, .. } => {
                metadata.is_symlink()
                    && std::fs::read_link(&path).map_err(at(&path))? == Path::new(target)
                    && std::fs::canonicalize(&path)
                        .map_err(at(&path))?
                        .starts_with(&canonical)
            }
        };
        if !valid {
            return Err(StoreError::Location(format!(
                "directory member {} does not match its manifest",
                path.display()
            )));
        }
    }
    Ok(())
}

/// Copies the files of a folder of this machine into another, where they are not yet.
async fn copy_folder(
    source: PathBuf,
    temporary: tempfile::TempDir,
) -> Result<tempfile::TempDir, StoreError> {
    tokio::task::spawn_blocking(move || {
        let mut pending = vec![(source, temporary.path().join("input"))];
        while let Some((from, to)) = pending.pop() {
            std::fs::create_dir_all(&to).map_err(at(&to))?;
            for entry in std::fs::read_dir(&from).map_err(at(&from))? {
                let entry = entry.map_err(at(&from))?;
                let inside = to.join(entry.file_name());
                if entry.file_type()?.is_symlink()
                    && !std::fs::metadata(entry.path()).is_ok_and(|metadata| metadata.is_file())
                {
                    let target = std::fs::read_link(entry.path())?;
                    std::os::unix::fs::symlink(target, &inside).map_err(at(&inside))?;
                    continue;
                }
                match entry.file_type().map_err(at(&inside))?.is_dir() {
                    true => pending.push((entry.path(), inside)),
                    false => {
                        std::fs::copy(entry.path(), &inside).map_err(at(&inside))?;
                    }
                }
            }
        }
        Ok(temporary)
    })
    .await
    .map_err(|error| std::io::Error::other(error.to_string()))?
}

/// An error of a file of this machine, with its path.
fn at(path: &Path) -> impl FnOnce(std::io::Error) -> StoreError + '_ {
    move |error| StoreError::File {
        path: path.to_owned(),
        error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cleans_failed_publication() {
        let operator = Operator::new(opendal::services::Memory::default()).unwrap();
        operator
            .write("content", Bytes::from_static(b"new"))
            .await
            .unwrap();
        let folder = tempfile::tempdir().unwrap();
        let target = folder.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("previous"), b"old").unwrap();
        assert!(download(&operator, "content", &target).await.is_err());
        assert_eq!(std::fs::read(target.join("previous")).unwrap(), b"old");
        assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 1);
    }

    #[test]
    fn cancellation_cleans_tree() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let operator = Operator::new(opendal::services::Memory::default()).unwrap();
            operator
                .write("member", Bytes::from(vec![b'x'; 1024 * 1024]))
                .await
                .unwrap();
            let folder = tempfile::tempdir().unwrap();
            let (entered, blocked) = tokio::sync::oneshot::channel();
            let (release, wait) = std::sync::mpsc::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                entered.send(()).unwrap();
                wait.recv().unwrap();
            });
            blocked.await.unwrap();
            let temporary = tempfile::tempdir_in(folder.path()).unwrap();
            let private = temporary.path().to_owned();
            let (started, ready) = tokio::sync::oneshot::channel();
            let transfer = tokio::spawn(async move {
                let target = temporary.path().join("input/member");
                started.send(()).unwrap();
                let result = download(&operator, "member", &target).await;
                drop(temporary);
                result
            });
            ready.await.unwrap();
            assert_eq!(std::fs::read_dir(private.join("input")).unwrap().count(), 1);
            transfer.abort();
            assert!(transfer.await.unwrap_err().is_cancelled());
            release.send(()).unwrap();
            blocker.await.unwrap();
            tokio::task::spawn_blocking(|| ()).await.unwrap();
            assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0);
        });
    }
}
