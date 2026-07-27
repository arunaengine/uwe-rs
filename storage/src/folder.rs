//! A folder on this machine that holds the objects of the addresses it serves: the rest of an
//! address after the prefix is the path inside the folder. Local `file://` addresses are a
//! folder too, the root of the file system.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use crate::error::Problem;
use crate::{Downloaded, Entry, Metadata, address};

pub(crate) struct Folder {
    pub root: PathBuf,
}

impl Folder {
    /// The path of an address that `prefix` serves. A name `..` could leave the folder, so it
    /// is not allowed.
    pub fn path(&self, prefix: &str, target: &str) -> Result<PathBuf, Problem> {
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
        match tokio::fs::copy(path, target).await {
            Ok(_) => Ok(Downloaded {
                version: std::fs::metadata(path).ok().as_ref().and_then(modified),
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(Problem::missing()),
            Err(error) => Err(Problem::local(path, error)),
        }
    }

    /// Copies a local file into the folder, through a temporary name, so that a reader never
    /// sees half a file.
    pub async fn upload(&self, file: &Path, path: &Path) -> Result<(), Problem> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| Problem::local(parent, error))?;
        }
        let mut part = path.as_os_str().to_owned();
        part.push(format!(".{}.part", std::process::id()));
        let part = PathBuf::from(part);
        tokio::fs::copy(file, &part)
            .await
            .map_err(|error| Problem::local(file, error))?;
        tokio::fs::rename(&part, path)
            .await
            .map_err(|error| Problem::local(path, error))
    }
}

/// The time of the last change of a file, in nanoseconds since the Unix epoch, as its version.
fn modified(found: &std::fs::Metadata) -> Option<String> {
    let modified = found.modified().ok()?;
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(format!("modified {}", since.as_nanos()))
}
