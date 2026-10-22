//! Reads and delivers task files through their local or executor-provided locations.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How a delivered file reaches its target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    /// A copy of its own, which file systems like btrfs and XFS make without copying blocks.
    #[default]
    Copy,
    /// A symbolic link to the stored file, by its absolute path.
    Link,
    /// A symbolic link to the stored file, by a path relative to the target.
    RelativeLink,
    /// A hard link to the stored file; a copy where no hard link is possible.
    HardLink,
}

/// A file or directory that a task made, and where it goes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delivery {
    /// The location of the output, as its outcome gives it.
    pub location: String,
    /// Where it goes: a path on this machine, or an address of a storage provider, like
    /// `s3://bucket/results/out.txt`.
    pub target: String,
    pub mode: DeliveryMode,
    /// Whether existing destinations may be replaced.
    #[serde(
        default = "overwrite_default",
        skip_serializing_if = "overwrite_enabled"
    )]
    pub overwrite: bool,
}

fn overwrite_default() -> bool {
    true
}

fn overwrite_enabled(value: &bool) -> bool {
    *value
}

/// The files that the tasks of a run made, wherever the executor keeps them.
pub trait Files: Send + Sync {
    /// A local path with the content of a location.
    fn local(&self, location: &str) -> io::Result<PathBuf>;

    /// Delivers a file or a directory to its target.
    fn deliver(&self, delivery: &Delivery) -> io::Result<()>;
}

/// Files whose locations are paths on this machine, as in a local store.
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalFiles;

impl Files for LocalFiles {
    fn local(&self, location: &str) -> io::Result<PathBuf> {
        Ok(PathBuf::from(location))
    }

    fn deliver(&self, delivery: &Delivery) -> io::Result<()> {
        let source = Path::new(&delivery.location);
        let target = Path::new(&delivery.target);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if !delivery.overwrite {
            return match create_delivery(source, target, delivery.mode) {
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
                result => result,
            };
        }
        remove(target)?;
        match delivery.mode {
            DeliveryMode::Copy => copy(source, target),
            DeliveryMode::Link => std::os::unix::fs::symlink(source, target),
            DeliveryMode::RelativeLink => {
                let base = target.parent().unwrap_or(Path::new("/"));
                std::os::unix::fs::symlink(relative(source, base), target)
            }
            DeliveryMode::HardLink => hard_link(source, target),
        }
    }
}

fn create_delivery(source: &Path, target: &Path, mode: DeliveryMode) -> io::Result<()> {
    match mode {
        DeliveryMode::Link => return std::os::unix::fs::symlink(source, target),
        DeliveryMode::RelativeLink => {
            let base = target.parent().unwrap_or(Path::new("/"));
            return std::os::unix::fs::symlink(relative(source, base), target);
        }
        _ => {}
    }
    match std::fs::symlink_metadata(target) {
        Ok(_) => return Err(io::ErrorKind::AlreadyExists.into()),
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let parent = target.parent().filter(|path| !path.as_os_str().is_empty());
    let directory = tempfile::tempdir_in(parent.unwrap_or(Path::new(".")))?;
    let staged = directory.path().join("output");
    match mode {
        DeliveryMode::HardLink => hard_link(source, &staged)?,
        _ => copy(source, &staged)?,
    }
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        &staged,
        rustix::fs::CWD,
        target,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(Into::into)
}

/// The content of a location that a task made: through `files` when the run has them, as a
/// store elsewhere needs, else from the local path that the location is.
pub fn read(files: Option<&dyn Files>, location: &str) -> io::Result<Vec<u8>> {
    match files {
        Some(files) => std::fs::read(files.local(location)?),
        None => std::fs::read(location),
    }
}

/// Removes a file, a link or a directory, if there is one.
fn remove(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Copies a file or a directory with all it holds.
fn copy(source: &Path, target: &Path) -> io::Result<()> {
    let metadata = std::fs::metadata(source)?;
    if std::fs::symlink_metadata(source)?.is_symlink() && metadata.is_dir() {
        return std::os::unix::fs::symlink(std::fs::read_link(source)?, target);
    }
    if !metadata.is_dir() {
        std::fs::copy(source, target)?;
        // Store files are read-only; a copy is the user's own file to change.
        let mode = metadata.permissions().mode() | 0o200;
        return std::fs::set_permissions(target, std::fs::Permissions::from_mode(mode));
    }
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        copy(&entry.path(), &target.join(entry.file_name()))?;
    }
    Ok(())
}

/// Hard-links a file, or each file of a directory; copies where no hard link is possible.
fn hard_link(source: &Path, target: &Path) -> io::Result<()> {
    let metadata = std::fs::metadata(source)?;
    if std::fs::symlink_metadata(source)?.is_symlink() && metadata.is_dir() {
        return std::os::unix::fs::symlink(std::fs::read_link(source)?, target);
    }
    if !metadata.is_dir() {
        return std::fs::hard_link(source, target).or_else(|_| copy(source, target));
    }
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        hard_link(&entry.path(), &target.join(entry.file_name()))?;
    }
    Ok(())
}

/// The path of `path` seen from the folder `base`; both are absolute.
fn relative(path: &Path, base: &Path) -> PathBuf {
    let path: Vec<_> = path.components().collect();
    let base: Vec<_> = base.components().collect();
    let common = path.iter().zip(&base).take_while(|(a, b)| a == b).count();
    let mut relative = PathBuf::new();
    for _ in common..base.len() {
        relative.push("..");
    }
    for component in &path[common..] {
        relative.push(component);
    }
    relative
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_existing_targets() {
        let folder = tempfile::tempdir().unwrap();
        let source = folder.path().join("source");
        let target = folder.path().join("target");
        std::fs::write(&source, "new").unwrap();
        for mode in [
            DeliveryMode::Copy,
            DeliveryMode::Link,
            DeliveryMode::RelativeLink,
            DeliveryMode::HardLink,
        ] {
            std::fs::write(&target, "existing").unwrap();
            let delivery = Delivery {
                location: source.to_string_lossy().into_owned(),
                target: target.to_string_lossy().into_owned(),
                mode,
                overwrite: false,
            };
            LocalFiles.deliver(&delivery).unwrap();
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "existing");
            std::fs::remove_file(&target).unwrap();
            LocalFiles.deliver(&delivery).unwrap();
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
            std::fs::remove_file(&target).unwrap();
        }
        let old: Delivery =
            serde_json::from_value(serde_json::json!({"location":"a","target":"b","mode":"copy"}))
                .unwrap();
        assert!(old.overwrite);
    }

    #[test]
    fn retries_failed_delivery() {
        for mode in [DeliveryMode::Copy, DeliveryMode::HardLink] {
            let folder = tempfile::tempdir().unwrap();
            let source = folder.path().join("source");
            let target = folder.path().join("target");
            std::fs::create_dir(&source).unwrap();
            std::fs::write(source.join("good"), b"complete").unwrap();
            std::os::unix::fs::symlink("missing", source.join("bad")).unwrap();
            let delivery = Delivery {
                location: source.to_string_lossy().into_owned(),
                target: target.to_string_lossy().into_owned(),
                mode,
                overwrite: false,
            };
            assert!(LocalFiles.deliver(&delivery).is_err());
            assert!(!target.exists());
            assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 1);
            std::fs::remove_file(source.join("bad")).unwrap();
            std::fs::write(source.join("bad"), b"repaired").unwrap();
            LocalFiles.deliver(&delivery).unwrap();
            assert_eq!(std::fs::read(target.join("good")).unwrap(), b"complete");
            assert_eq!(std::fs::read(target.join("bad")).unwrap(), b"repaired");
            std::fs::remove_dir_all(&target).unwrap();
            std::fs::write(&target, b"another owner").unwrap();
            std::fs::remove_dir_all(&source).unwrap();
            LocalFiles.deliver(&delivery).unwrap();
            assert_eq!(std::fs::read(&target).unwrap(), b"another owner");
        }
    }

    #[test]
    fn finds_common_folder() {
        let path = relative(Path::new("/s/runs/a/out.txt"), Path::new("/home/results/x"));
        assert_eq!(path, PathBuf::from("../../../s/runs/a/out.txt"));
        let path = relative(Path::new("/r/a/b.txt"), Path::new("/r"));
        assert_eq!(path, PathBuf::from("a/b.txt"));
    }

    #[test]
    fn replaces_delivery_targets() {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let stored = folder.path().join("store/out.txt");
        std::fs::create_dir_all(stored.parent().unwrap()).unwrap();
        std::fs::write(&stored, "made\n").unwrap();
        std::fs::set_permissions(&stored, std::fs::Permissions::from_mode(0o444)).unwrap();
        let tree = folder.path().join("store/db");
        std::fs::create_dir_all(tree.join("sub")).unwrap();
        std::fs::write(tree.join("sub/a.txt"), "a\n").unwrap();
        let deliver = |location: &Path, target: &str, mode| {
            let delivery = Delivery {
                location: location.to_string_lossy().into_owned(),
                target: folder.path().join(target).to_string_lossy().into_owned(),
                mode,
                overwrite: true,
            };
            LocalFiles.deliver(&delivery).expect("delivers");
            folder.path().join(target)
        };
        let copied = deliver(&stored, "results/out.txt", DeliveryMode::Copy);
        assert_eq!(std::fs::read_to_string(&copied).unwrap(), "made\n");
        let mode = std::fs::metadata(&copied).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o644,
            "a copy of a read-only store file is writable"
        );
        // A second delivery to the same target replaces the first.
        let linked = deliver(&stored, "results/out.txt", DeliveryMode::Link);
        assert_eq!(std::fs::read_link(&linked).unwrap(), stored);
        let relative = deliver(&stored, "results/rel.txt", DeliveryMode::RelativeLink);
        assert_eq!(
            std::fs::read_link(&relative).unwrap(),
            PathBuf::from("../store/out.txt")
        );
        let tree_copy = deliver(&tree, "results/db", DeliveryMode::HardLink);
        assert_eq!(
            std::fs::read_to_string(tree_copy.join("sub/a.txt")).unwrap(),
            "a\n"
        );
    }

    #[test]
    fn preserves_directory_aliases() {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let source = folder.path().join("source");
        std::fs::create_dir_all(source.join("sub")).unwrap();
        std::fs::write(source.join("sub/member"), "nested\n").unwrap();
        std::os::unix::fs::symlink("sub", source.join("alias")).unwrap();

        for (name, mode) in [
            ("copy", DeliveryMode::Copy),
            ("hard-link", DeliveryMode::HardLink),
        ] {
            let target = folder.path().join(name);
            LocalFiles
                .deliver(&Delivery {
                    location: source.to_string_lossy().into_owned(),
                    target: target.to_string_lossy().into_owned(),
                    mode,
                    overwrite: true,
                })
                .expect("delivers an alias");
            assert!(
                std::fs::symlink_metadata(target.join("alias"))
                    .unwrap()
                    .is_symlink()
            );
            assert_eq!(
                std::fs::read_link(target.join("alias")).unwrap(),
                PathBuf::from("sub")
            );
            assert_eq!(
                std::fs::read_to_string(target.join("alias/member")).unwrap(),
                "nested\n"
            );
        }
    }

    #[test]
    fn preserves_directory_cycles() {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let source = folder.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("member"), "content\n").unwrap();
        std::os::unix::fs::symlink(".", source.join("cycle")).unwrap();

        for (name, mode) in [
            ("copy", DeliveryMode::Copy),
            ("hard-link", DeliveryMode::HardLink),
        ] {
            let target = folder.path().join(name);
            LocalFiles
                .deliver(&Delivery {
                    location: source.to_string_lossy().into_owned(),
                    target: target.to_string_lossy().into_owned(),
                    mode,
                    overwrite: true,
                })
                .expect("delivers a cycle");
            assert!(
                std::fs::symlink_metadata(target.join("cycle"))
                    .unwrap()
                    .is_symlink()
            );
            assert_eq!(
                std::fs::read_link(target.join("cycle")).unwrap(),
                PathBuf::from(".")
            );
            assert_eq!(
                std::fs::read_to_string(target.join("member")).unwrap(),
                "content\n"
            );
        }
    }
}
