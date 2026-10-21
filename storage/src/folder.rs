//! Maps provider address suffixes to files in a local folder.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use crate::error::Problem;
use crate::{Downloaded, Entry, Metadata, Written, address};

pub(crate) struct Folder {
    pub root: PathBuf,
}

impl Folder {
    /// The path of an address that `prefix` serves. A name `..` could leave the folder, so it
    /// is not allowed.
    pub fn path(&self, prefix: &str, target: &str) -> Result<PathBuf, Problem> {
        if target.starts_with("s3://") {
            match address::object_location(target) {
                Ok(Some(location)) if location.version.is_some() => {
                    return Err(Problem::fixed(
                        "a folder provider cannot select or write an S3 object version",
                    ));
                }
                Err(message) => return Err(Problem::fixed(message)),
                _ => {}
            }
        }
        let rest = target.strip_prefix(prefix).unwrap_or_default();
        let rest = rest.split(['?', '#']).next().unwrap_or_default();
        let mut path = self.root.clone();
        for part in rest
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
        {
            let part = address::decode(part);
            if part == ".." || part.contains('/') {
                return Err(Problem::fixed(
                    "the address leaves the folder of its provider",
                ));
            }
            path.push(part);
        }
        Ok(path)
    }

    pub async fn stat(&self, path: &Path) -> Result<Option<Metadata>, Problem> {
        match tokio::fs::metadata(path).await {
            Ok(found) => Ok(Some(Metadata {
                directory: found.is_dir(),
                modified: found.modified().ok(),
                size: if found.is_dir() { 0 } else { found.len() },
                version: modified(&found),
            })),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Problem::local(path, error)),
        }
    }

    pub async fn list(&self, path: &Path) -> Result<Vec<Entry>, Problem> {
        let mut entries = Vec::new();
        let mut folder = tokio::fs::read_dir(path)
            .await
            .map_err(|error| Problem::local(path, error))?;
        while let Some(entry) = folder
            .next_entry()
            .await
            .map_err(|error| Problem::local(path, error))?
        {
            let directory = tokio::fs::metadata(entry.path())
                .await
                .is_ok_and(|found| found.is_dir());
            entries.push(Entry {
                name: entry.file_name().to_string_lossy().into_owned(),
                directory,
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    /// Copies a file to `target`, whole: a copy of a local file is fast, and file systems like
    /// btrfs and XFS share its blocks instead of copying them.
    pub async fn download(&self, path: &Path, target: &Path) -> Result<Downloaded, Problem> {
        let parent = target
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let part = tempfile::NamedTempFile::new_in(parent)
            .map_err(|error| Problem::local(target, error))?;
        let source = path.to_owned();
        let downloaded =
            tokio::task::spawn_blocking(move || download_copy(std::fs::File::open(source)?, part))
                .await
                .map_err(|error| Problem::local(path, std::io::Error::other(error)))?;
        match downloaded {
            Ok((part, version)) => {
                part.persist(target)
                    .map_err(|error| Problem::local(target, error.error))?;
                Ok(Downloaded { version })
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(Problem::missing()),
            Err(error) => Err(Problem::local(path, error)),
        }
    }

    /// Copies a local file into the folder, through a temporary name, so that a reader never
    /// sees half a file.
    pub async fn upload(&self, file: &Path, path: &Path) -> Result<Written, Problem> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| Problem::local(parent, error))?;
        }
        let parent = path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let part =
            tempfile::NamedTempFile::new_in(parent).map_err(|error| Problem::local(path, error))?;
        let part = copied(file, part)
            .await
            .map_err(|error| Problem::local(file, error))?;
        let size = part
            .as_file()
            .metadata()
            .map_err(|error| Problem::local(file, error))?
            .len();
        part.persist(path)
            .map_err(|error| Problem::local(path, error.error))?;
        Ok(Written {
            size,
            version: None,
        })
    }
}

fn download_copy(
    mut source: std::fs::File,
    mut part: tempfile::NamedTempFile,
) -> std::io::Result<(tempfile::NamedTempFile, Option<String>)> {
    let before = source.metadata()?;
    let version = modified(&before);
    part.as_file().set_permissions(before.permissions())?;
    let size = std::io::copy(&mut source, part.as_file_mut())?;
    let after = source.metadata()?;
    if size != before.len() || after.len() != before.len() || modified(&after) != version {
        return Err(std::io::Error::other(
            "the source file changed while being downloaded",
        ));
    }
    Ok((part, version))
}

/// Keeps the private file alive until even a detached blocking copy has finished.
pub(crate) async fn copied(
    file: &Path,
    part: tempfile::NamedTempFile,
) -> std::io::Result<tempfile::NamedTempFile> {
    let file = file.to_owned();
    tokio::task::spawn_blocking(move || {
        std::fs::copy(file, part.path())?;
        Ok(part)
    })
    .await
    .map_err(std::io::Error::other)?
}

/// The time of the last change of a file, in nanoseconds since the Unix epoch, as its version.
fn modified(found: &std::fs::Metadata) -> Option<String> {
    let modified = found.modified().ok()?;
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(format!("modified {}", since.as_nanos()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_observed_version() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("source");
        std::fs::write(&path, b"old").unwrap();
        let source = std::fs::File::open(&path).unwrap();
        source
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1)),
            )
            .unwrap();
        let expected = modified(&source.metadata().unwrap());
        let replacement = folder.path().join("replacement");
        std::fs::write(&replacement, b"new").unwrap();
        std::fs::rename(replacement, &path).unwrap();
        let part = tempfile::NamedTempFile::new_in(folder.path()).unwrap();
        let (part, version) = download_copy(source, part).unwrap();
        assert_eq!(std::fs::read(part.path()).unwrap(), b"old");
        assert_eq!(version, expected);
        assert_ne!(version, modified(&std::fs::metadata(path).unwrap()));
    }
}
