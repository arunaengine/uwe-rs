//! Publishes local store files atomically through owned temporary paths.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{ErrorKind, Write};
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

/// Copies a file to `target` unless it holds that content already. The copy goes to a
/// temporary name first. A plain copy lets file systems like btrfs and XFS share the blocks.
pub(crate) async fn copy_absent(source: &Path, target: &Path) -> std::io::Result<()> {
    let source = source.to_owned();
    let target = target.to_owned();
    tokio::task::spawn_blocking(move || {
        let size = std::fs::metadata(&source)?.len();
        if present(&target, size, || same_bytes(&source, &target))? {
            return Ok(());
        }
        create_parent(&target)?;
        let part = temporary(&target)?;
        std::fs::copy(source, part.path())?;
        publish(part, &target)
    })
    .await
    .map_err(|error| std::io::Error::other(error.to_string()))?
}

/// Writes bytes to `target` unless it holds them already, through a temporary name.
pub(crate) async fn write_absent(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let target = target.to_owned();
    let bytes = bytes.to_owned();
    tokio::task::spawn_blocking(move || {
        let size = bytes.len() as u64;
        if present(&target, size, || Ok(std::fs::read(&target)? == bytes))? {
            return Ok(());
        }
        create_parent(&target)?;
        let mut part = temporary(&target)?;
        part.write_all(&bytes)?;
        publish(part, &target)
    })
    .await
    .map_err(|error| std::io::Error::other(error.to_string()))?
}

/// Writes bytes to `target` through a temporary name, replacing what is there, so that tasks
/// that write the same file at once never mix their bytes.
pub(crate) async fn replace(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let target = target.to_owned();
    let bytes = bytes.to_owned();
    tokio::task::spawn_blocking(move || {
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
                Ok(()) => return settle(&target),
                Err(error) if error.kind() == ErrorKind::CrossesDevices => {}
                Err(error) => return Err(error),
            }
        }
        let part = temporary(&target)?;
        std::fs::copy(source, part.path())?;
        publish(part, &target)
    })
    .await
    .map_err(|error| std::io::Error::other(error.to_string()))?
}

/// Gives `target` the content of `source` by a hard link, unless it holds that content
/// already. A damaged `target` is replaced. Where no link is possible, it copies.
pub(crate) async fn link_absent(source: &Path, target: &Path) -> std::io::Result<()> {
    let source = source.to_owned();
    let target = target.to_owned();
    tokio::task::spawn_blocking(move || {
        let size = std::fs::metadata(&source)?.len();
        if present(&target, size, || same_bytes(&source, &target))? {
            return Ok(());
        }
        create_parent(&target)?;
        let parent = target.parent().unwrap_or_else(|| Path::new("."));
        let linked = tempfile::Builder::new()
            .prefix(".part-")
            .make_in(parent, |name| std::fs::hard_link(&source, name));
        match linked {
            Ok(part) => {
                part.persist(&target).map_err(|error| error.error)?;
                sync_folder(&target)
            }
            Err(_) => {
                let part = temporary(&target)?;
                std::fs::copy(&source, part.path())?;
                publish(part, &target)
            }
        }
    })
    .await
    .map_err(|error| std::io::Error::other(error.to_string()))?
}

/// Whether `target` is a whole copy of `size` bytes that may stay. A file that is still
/// writable may have been changed, so `same` compares its bytes; one that passes turns
/// read-only, so that later checks trust its size.
fn present(
    target: &Path,
    size: u64,
    same: impl FnOnce() -> std::io::Result<bool>,
) -> std::io::Result<bool> {
    let metadata = match std::fs::symlink_metadata(target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() || metadata.len() != size {
        return Ok(false);
    }
    let mode = metadata.permissions().mode();
    if mode & 0o222 == 0 {
        return Ok(true);
    }
    if !same()? {
        return Ok(false);
    }
    read_only(target, mode)?;
    Ok(true)
}

/// Whether two files hold the same bytes, read in parts.
fn same_bytes(first: &Path, second: &Path) -> std::io::Result<bool> {
    use std::io::Read;
    let (mut first, mut second) = (std::fs::File::open(first)?, std::fs::File::open(second)?);
    let (mut left, mut right) = (vec![0; 1 << 20], vec![0; 1 << 20]);
    loop {
        let read = first.read(&mut left)?;
        if read == 0 {
            return Ok(second.read(&mut right[..1])? == 0);
        }
        if second.read_exact(&mut right[..read]).is_err() || left[..read] != right[..read] {
            return Ok(false);
        }
    }
}

/// Makes a file read-only, so that a write through a link to a published result cannot
/// change the store; a file of another user, like one of a task, keeps its mode.
fn read_only(path: &Path, mode: u32) -> std::io::Result<()> {
    match std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & !0o222)) {
        Err(error) if error.kind() != ErrorKind::PermissionDenied => Err(error),
        _ => Ok(()),
    }
}

/// Puts a complete temporary file in place: read-only and on disk first, so that a power loss
/// leaves either no file or the whole one.
fn publish(part: tempfile::NamedTempFile, target: &Path) -> std::io::Result<()> {
    let mode = part.as_file().metadata()?.permissions().mode();
    read_only(part.path(), mode)?;
    part.as_file().sync_all()?;
    part.persist(target).map_err(|error| error.error)?;
    sync_folder(target)
}

/// Makes a file that was moved into place read-only and puts it and its folder on disk.
fn settle(target: &Path) -> std::io::Result<()> {
    read_only(target, std::fs::metadata(target)?.permissions().mode())?;
    if let Ok(file) = std::fs::File::open(target) {
        file.sync_all()?;
    }
    sync_folder(target)
}

/// Puts the folder entry of a new name on disk.
fn sync_folder(target: &Path) -> std::io::Result<()> {
    match target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        Some(parent) => std::fs::File::open(parent)?.sync_all(),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn failure_preserves_data() {
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
    async fn preserves_source_permissions() {
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
                0o551,
                "store files keep their program bits and lose their write bits"
            );
        }
        assert!(link.is_symlink());
    }

    #[tokio::test]
    async fn repairs_damaged_copies() {
        let folder = tempfile::tempdir().unwrap();
        let target = folder.path().join("cas/object");
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let damage = |bytes: &[u8]| {
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
            std::fs::write(&target, bytes).unwrap();
        };
        write_absent(&target, b"content").await.unwrap();
        assert_eq!(mode(&target), 0o444, "a stored file is read-only");
        assert!(
            std::fs::OpenOptions::new()
                .append(true)
                .open(&target)
                .is_err()
        );
        // A power loss left an empty file; the next write stores the content again.
        damage(b"");
        write_absent(&target, b"content").await.unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"content");
        // Someone changed the bytes through a published link.
        damage(b"CONTENT");
        let source = folder.path().join("source");
        std::fs::write(&source, b"content").unwrap();
        copy_absent(&source, &target).await.unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"content");
        assert_eq!(mode(&target), 0o444);
        damage(b"content and more");
        link_absent(&source, &target).await.unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"content");
        // A writable file with the right bytes stays, and becomes read-only.
        damage(b"content");
        write_absent(&target, b"content").await.unwrap();
        assert_eq!(mode(&target), 0o444);
        let names = std::fs::read_dir(target.parent().unwrap()).unwrap().count();
        assert_eq!(names, 1, "no temporary file stays behind");
    }

    #[test]
    fn owns_cancelled_copy() {
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
