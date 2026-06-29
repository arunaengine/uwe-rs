//! File operations of a store on the local file system: files arrive under a temporary name
//! and are renamed into place, so that a reader never sees half a file; task outputs are moved
//! in and their content is hard-linked, so that nothing is copied.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Numbers the temporary files of this process, so that two copies never share a name.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// A name next to `target` that no other copy uses.
fn temporary(target: &Path) -> PathBuf {
    let number = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut name = target.as_os_str().to_owned();
    name.push(format!(".part-{}-{number}", std::process::id()));
    PathBuf::from(name)
}

async fn create_parent(target: &Path) -> std::io::Result<()> {
    match target.parent() {
        Some(parent) => tokio::fs::create_dir_all(parent).await,
        None => Ok(()),
    }
}

/// Copies a file to `target` unless it exists. The copy goes to a temporary name first. A plain
/// copy lets file systems like btrfs and XFS share the blocks instead of copying them.
pub(crate) async fn copy_absent(source: &Path, target: &Path) -> std::io::Result<()> {
    if tokio::fs::try_exists(target).await? {
        return Ok(());
    }
    create_parent(target).await?;
    let part = temporary(target);
    tokio::fs::copy(source, &part).await?;
    tokio::fs::rename(&part, target).await
}

/// Writes bytes to `target` unless it exists, through a temporary name.
pub(crate) async fn write_absent(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if tokio::fs::try_exists(target).await? {
        return Ok(());
    }
    create_parent(target).await?;
    let part = temporary(target);
    tokio::fs::write(&part, bytes).await?;
    tokio::fs::rename(&part, target).await
}

/// Moves a file to `target`, replacing what is there. A symbolic link is replaced by a copy of
/// what it points to, and a move to another file system becomes a copy.
pub(crate) async fn move_file(source: &Path, target: &Path) -> std::io::Result<()> {
    create_parent(target).await?;
    let link = tokio::fs::symlink_metadata(source).await?.is_symlink();
    if !link {
        match tokio::fs::rename(source, target).await {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == ErrorKind::CrossesDevices => {}
            Err(error) => return Err(error),
        }
    }
    let part = temporary(target);
    tokio::fs::copy(source, &part).await?;
    tokio::fs::rename(&part, target).await
}

/// Gives `target` the content of `source` by a hard link, unless `target` exists. Where no link
/// is possible, like across file systems, it copies.
pub(crate) async fn link_absent(source: &Path, target: &Path) -> std::io::Result<()> {
    if tokio::fs::try_exists(target).await? {
        return Ok(());
    }
    create_parent(target).await?;
    match tokio::fs::hard_link(source, target).await {
        Ok(()) => Ok(()),
        // Another task stored the same content at the same moment.
        Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(_) => copy_absent(source, target).await,
    }
}
