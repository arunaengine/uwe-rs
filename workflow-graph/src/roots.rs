//! Keeps the local files that workflow code reads and writes inside the folders of its run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io;
use std::path::{Path, PathBuf};

/// The folders that workflow code of an isolated run may reach, with their links resolved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Roots {
    folders: Vec<PathBuf>,
    /// Shared folders, like the cache of all runs, whose files workflow code only reads.
    read_only: Vec<PathBuf>,
}

impl Roots {
    /// A folder that does not exist yet counts by where its path leads.
    pub fn new(folders: impl IntoIterator<Item = PathBuf>) -> Roots {
        let folders = folders.into_iter().filter_map(|folder| resolve(&folder));
        Roots {
            folders: folders.collect(),
            read_only: Vec::new(),
        }
    }

    /// Adds folders whose files workflow code may read by their paths, but not list or change.
    pub fn with_read_only(mut self, folders: impl IntoIterator<Item = PathBuf>) -> Roots {
        let folders = folders.into_iter().filter_map(|folder| resolve(&folder));
        self.read_only.extend(folders);
        self
    }

    /// Refuses a path that leads outside the folders, also through a link.
    pub fn check(&self, path: &Path) -> io::Result<()> {
        if resolve(path).is_some_and(|real| self.folders.iter().any(|root| real.starts_with(root)))
        {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "workflow code may not reach {}: it is outside the folders of the run",
                path.display()
            ),
        ))
    }

    /// Like [`Roots::check`], but also lets a path in a read-only folder through, to read it.
    pub fn check_read(&self, path: &Path) -> io::Result<()> {
        if resolve(path)
            .is_some_and(|real| self.read_only.iter().any(|root| real.starts_with(root)))
        {
            return Ok(());
        }
        self.check(path)
    }
}

/// Where a path leads: its existing part with links resolved, then the rest. `None` when the
/// rest climbs with `..` or starts with a broken link, whose target is not known.
fn resolve(path: &Path) -> Option<PathBuf> {
    let path = std::path::absolute(path).ok()?;
    let mut rest = Vec::new();
    let mut existing = path.as_path();
    loop {
        if let Ok(real) = existing.canonicalize() {
            return Some(rest.iter().rev().fold(real, |path, name| path.join(name)));
        }
        if existing.symlink_metadata().is_ok() {
            return None;
        }
        rest.push(existing.file_name()?);
        existing = existing.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_outside_paths() {
        let folder = tempfile::tempdir().unwrap();
        let run = folder.path().join("run");
        let other = folder.path().join("other");
        std::fs::create_dir_all(run.join("data")).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::os::unix::fs::symlink(&other, run.join("away")).unwrap();
        std::os::unix::fs::symlink(other.join("gone"), run.join("broken")).unwrap();
        let roots = Roots::new([run.clone(), folder.path().join("later")]);
        for allowed in [
            run.join("data"),
            run.join("data/new/file.txt"),
            run.join("data/../data/file.txt"),
            folder.path().join("later/file.txt"),
        ] {
            assert!(roots.check(&allowed).is_ok(), "{}", allowed.display());
        }
        for refused in [
            other.join("file.txt"),
            run.join("away/file.txt"),
            run.join("broken"),
            run.join("data/../../other/file.txt"),
            run.join("missing/../../other"),
            PathBuf::from("/etc/passwd"),
        ] {
            let error = roots
                .check(&refused)
                .expect_err(&refused.display().to_string());
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert!(error.to_string().contains("outside the folders of the run"));
        }
        assert!(Roots::default().check(&run).is_err());
    }

    #[test]
    fn reads_shared_folders() {
        let folder = tempfile::tempdir().unwrap();
        let run = folder.path().join("cache/scratch/launch/run");
        let shared = folder.path().join("cache");
        std::fs::create_dir_all(&run).unwrap();
        let roots = Roots::new([run.clone()]).with_read_only([shared.clone()]);
        assert!(roots.check(&run.join("file.txt")).is_ok());
        assert!(roots.check_read(&run.join("file.txt")).is_ok());
        assert!(roots.check_read(&shared.join("store/file.txt")).is_ok());
        assert!(roots.check(&shared.join("store/file.txt")).is_err());
        assert!(roots.check(&shared).is_err());
        assert!(roots.check_read(&folder.path().join("other.txt")).is_err());
    }
}
