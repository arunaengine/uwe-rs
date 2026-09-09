//! File operations of a store on the local file system: files arrive under a temporary name
//! and are renamed into place, so that a reader never sees half a file; task outputs are moved
//! in and their content is hard-linked, so that nothing is copied.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// A name next to `target` that no other copy uses.
fn temporary(target: &Path) -> std::io::Result<tempfile::NamedTempFile> {
    tempfile::Builder::new()
        .prefix(".part-")
        .permissions(std::fs::Permissions::from_mode(0o666))
        .tempfile_in(target.parent().unwrap_or_else(|| Path::new(".")))
}

fn create_parent(target: &Path) -> std::io::Result<()> {
    match target.parent() {
        Some(parent) => std::fs::create_dir_all(parent),
        None => Ok(()),
    }
}

/// Copies a file to `target` unless it exists. The copy goes to a temporary name first. A plain
/// copy lets file systems like btrfs and XFS share the blocks instead of copying them.
pub(crate) async fn copy_absent(source: &Path, target: &Path) -> std::io::Result<()> {
    let source = source.to_owned();
    let target = target.to_owned();
    tokio::task::spawn_blocking(move || {
        if target.try_exists()? {
            return Ok(());
        }
        create_parent(&target)?;
        let part = temporary(&target)?;
        std::fs::copy(source, part.path())?;
        part.persist(target)
            .map(|_| ())
            .map_err(|error| error.error)
    })
    .await
    .map_err(|error| std::io::Error::other(error.to_string()))?
}

/// Writes bytes to `target` unless it exists, through a temporary name.
pub(crate) async fn write_absent(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let target = target.to_owned();
    let bytes = bytes.to_owned();
    tokio::task::spawn_blocking(move || {
        if target.try_exists()? {
            return Ok(());
        }
        create_parent(&target)?;
        let part = temporary(&target)?;
        std::fs::write(part.path(), bytes)?;
        part.persist(target)
            .map(|_| ())
            .map_err(|error| error.error)
    })
    .await
    .map_err(|error| std::io::Error::other(error.to_string()))?
}

/// Moves a file to `target`, replacing what is there. A symbolic link is replaced by a copy of
/// what it points to, and a move to another file system becomes a copy.
pub(crate) async fn move_file(source: &Path, target: &Path) -> std::io::Result<()> {
    let source = source.to_owned();
    let target = target.to_owned();
    tokio::task::spawn_blocking(move || {
        create_parent(&target)?;
        let link = std::fs::symlink_metadata(&source)?.is_symlink();
        if !link {
            match std::fs::rename(&source, &target) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == ErrorKind::CrossesDevices => {}
                Err(error) => return Err(error),
            }
        }
        let part = temporary(&target)?;
        std::fs::copy(source, part.path())?;
        part.persist(target)
            .map(|_| ())
            .map_err(|error| error.error)
    })
    .await
    .map_err(|error| std::io::Error::other(error.to_string()))?
}

/// Gives `target` the content of `source` by a hard link, unless `target` exists. Where no link
/// is possible, like across file systems, it copies.
pub(crate) async fn link_absent(source: &Path, target: &Path) -> std::io::Result<()> {
    if tokio::fs::try_exists(target).await? {
        return Ok(());
    }
    create_parent(target)?;
    match tokio::fs::hard_link(source, target).await {
        Ok(()) => Ok(()),
        // Another task stored the same content at the same moment.
        Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(_) => copy_absent(source, target).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn failed_copies_and_moves_clean_owned_temporaries_and_preserve_previous_data() {
        let folder = tempfile::tempdir().unwrap();
        let missing = folder.path().join("missing");
        let target = folder.path().join("target");
        assert!(copy_absent(&missing, &target).await.is_err());
        assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0);
        let source = folder.path().join("source");
        std::fs::write(&source, b"new").unwrap();
        let link = folder.path().join("link");
        std::os::unix::fs::symlink(&source, &link).unwrap();
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("previous"), b"old").unwrap();
        assert!(move_file(&link, &target).await.is_err());
        assert_eq!(std::fs::read(target.join("previous")).unwrap(), b"old");
        assert_eq!(std::fs::read(&source).unwrap(), b"new");
        assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 3);
    }

    #[tokio::test]
    async fn copies_and_symlink_moves_preserve_source_permissions() {
        let folder = tempfile::tempdir().unwrap();
        let source = folder.path().join("source");
        std::fs::write(&source, b"executable").unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o751)).unwrap();
        let copy = folder.path().join("copy");
        copy_absent(&source, &copy).await.unwrap();
        let link = folder.path().join("link");
        std::os::unix::fs::symlink(&source, &link).unwrap();
        let moved = folder.path().join("moved");
        move_file(&link, &moved).await.unwrap();
        for path in [&copy, &moved] {
            assert_eq!(std::fs::read(path).unwrap(), b"executable");
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o751
            );
        }
        assert!(link.is_symlink());
    }

    #[test]
    fn dropping_a_queued_copy_does_not_abandon_its_temporary_owner() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let folder = tempfile::tempdir().unwrap();
            let source = folder.path().join("source");
            let target = folder.path().join("target");
            std::fs::write(&source, b"complete").unwrap();
            let (entered, blocked) = tokio::sync::oneshot::channel();
            let (release, wait) = std::sync::mpsc::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                entered.send(()).unwrap();
                wait.recv().unwrap();
            });
            blocked.await.unwrap();
            let (started, ready) = tokio::sync::oneshot::channel();
            let destination = target.clone();
            let copy = tokio::spawn(async move {
                started.send(()).unwrap();
                copy_absent(&source, &destination).await
            });
            ready.await.unwrap();
            copy.abort();
            assert!(copy.await.unwrap_err().is_cancelled());
            release.send(()).unwrap();
            blocker.await.unwrap();
            tokio::task::spawn_blocking(|| ()).await.unwrap();
            assert_eq!(std::fs::read(target).unwrap(), b"complete");
            assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 2);
        });
    }
}
