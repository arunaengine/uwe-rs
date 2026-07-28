//! Remote files downloaded once: a local copy lives in a folder of downloads under the SHA-256
//! of the file's public address, so that later requests and later runs take it from there.
//! What an earlier run found out about a remote file is noted there too.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::local;
use crate::journal::{self, Fetched};
use crate::{Metadata, Storage, StorageError, address};

/// The name of the local copy of a file in the folder of downloads: the SHA-256 of its public
/// web address where it has one, as runs before storage providers named it, else of its
/// address.
pub fn download_name(target: &str) -> String {
    let named = address::public_url(target).unwrap_or_else(|| target.to_owned());
    Sha256::digest(named.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl Storage {
    /// A local copy of a file, downloaded into `downloads` unless an earlier call did. A local
    /// path, or a `file://` address, is its own copy.
    pub async fn fetch(&self, target: &str, downloads: &Path) -> Result<PathBuf, StorageError> {
        if address::scheme(target).is_none() {
            return Ok(PathBuf::from(target));
        }
        if let Some(path) = target.strip_prefix("file://") {
            return Ok(PathBuf::from(address::decode(path)));
        }
        if let Some(pinned) = self.inner.journal.pinned(target) {
            let copy = pinned.copy.clone();
            self.inner.journal.note(pinned);
            return Ok(copy);
        }
        let name = download_name(target);
        let copy = downloads.join(&name);
        if !tokio::fs::try_exists(&copy).await.map_err(local(&copy))? {
            tokio::fs::create_dir_all(downloads)
                .await
                .map_err(local(downloads))?;
            // The file comes into a part named with the process id, which is renamed once
            // complete, so that runs that download the same file at once do not mix. A run
            // that was stopped leaves its part, which the next run takes over and continues.
            let partial = copy.with_extension(format!("{}.partial", std::process::id()));
            let present = take_over(downloads, &name, &partial);
            let downloaded = self.download_from(target, &partial, present).await?;
            tokio::fs::rename(&partial, &copy)
                .await
                .map_err(local(&copy))?;
            // Later runs compare the version with the one that the provider gives then. A
            // provider that gives none leaves the size to compare.
            journal::note_version(&copy, downloaded.version.as_deref());
        }
        self.inner
            .journal
            .note(journal::described(target, &copy).await?);
        Ok(copy)
    }

    /// Downloads a file into `file`, whole, as a transfer: the process does not note it as a
    /// file that it read, like a result that goes from a store elsewhere to its place.
    pub async fn download(&self, target: &str, file: &Path) -> Result<(), StorageError> {
        self.download_from(target, file, 0).await.map(|_| ())
    }

    /// A copy of a remote file at `file`, which the process notes as read, like a file of a
    /// workflow on the web that a run keeps next to the others; a pinned copy answers for its
    /// address.
    pub async fn copy_of(&self, target: &str, file: &Path) -> Result<(), StorageError> {
        let (fetched, version) = match self.inner.journal.pinned(target) {
            Some(pinned) => {
                tokio::fs::copy(&pinned.copy, file)
                    .await
                    .map_err(local(file))?;
                let version = pinned.version.clone();
                (pinned, version)
            }
            None => {
                let downloaded = self.download_from(target, file, 0).await?;
                (journal::described(target, file).await?, downloaded.version)
            }
        };
        self.inner.journal.note(Fetched { version, ..fetched });
        Ok(())
    }

    /// The remote files that this process read through the providers, each address once.
    pub fn fetched(&self) -> Vec<Fetched> {
        self.inner.journal.read()
    }

    /// Answers every later read of an address with a copy that an earlier run read, as a
    /// repeat of that run does.
    pub fn pin(&self, fetched: Fetched) {
        self.inner.journal.pin(fetched);
    }

    /// Whether a remote file exists, and its size. A file that an earlier run downloaded or
    /// found is known from `downloads`, without a request; a file that a request finds is
    /// noted there for later runs.
    pub async fn metadata(
        &self,
        target: &str,
        downloads: &Path,
    ) -> Result<Option<Metadata>, StorageError> {
        if let Some(pinned) = self.inner.journal.pinned(target) {
            return Ok(Some(Metadata {
                directory: false,
                size: pinned.size,
                version: pinned.version,
            }));
        }
        if let Some(known) = known(target, downloads) {
            return Ok(Some(known));
        }
        let found = self.stat(target).await?;
        if let Some(found) = &found {
            note(target, downloads, found);
        }
        Ok(found)
    }
}

/// Takes over the largest part of `name` that an earlier run left in `downloads` by renaming
/// it to `partial`, and gives its size; 0 when there is none. A part of a run that still goes
/// on, `<name>.<process id>.partial`, is left alone. Of two runs that try at once, only one
/// can rename a part.
fn take_over(downloads: &Path, name: &str, partial: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(downloads) else {
        return 0;
    };
    let left = |file: &str| {
        let Some(middle) = file
            .strip_prefix(&format!("{name}."))
            .and_then(|rest| rest.strip_suffix("partial"))
        else {
            return false;
        };
        // The part of a stopped run, or of the first version of iunctus that had no id.
        let owner = middle.trim_end_matches('.');
        owner.is_empty() || !Path::new("/proc").join(owner).exists()
    };
    let mut parts: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter(|entry| left(&entry.file_name().to_string_lossy()))
        .filter_map(|entry| Some((entry.metadata().ok()?.len(), entry.path())))
        .collect();
    parts.sort();
    while let Some((size, part)) = parts.pop() {
        if std::fs::rename(&part, partial).is_ok() {
            return size;
        }
    }
    0
}

/// What `downloads` knows of a remote file: the size of its copy, or what an earlier run noted.
fn known(target: &str, downloads: &Path) -> Option<Metadata> {
    let copy = downloads.join(download_name(target));
    if let Some(found) = std::fs::metadata(&copy)
        .ok()
        .filter(|found| found.is_file())
    {
        return Some(Metadata {
            directory: false,
            size: found.len(),
            version: None,
        });
    }
    let noted = std::fs::read_to_string(copy.with_extension("metadata")).ok()?;
    match noted.trim().split_once(' ') {
        Some(("file", size)) => Some(Metadata {
            directory: false,
            size: size.parse().ok()?,
            version: None,
        }),
        None if noted.trim() == "directory" => Some(Metadata {
            directory: true,
            size: 0,
            version: None,
        }),
        _ => None,
    }
}

/// Notes in `downloads` that a remote file exists, for later runs. The note is only a
/// shortcut, so a note that cannot be written is left out.
fn note(target: &str, downloads: &Path, found: &Metadata) {
    let text = match found.directory {
        true => "directory\n".to_owned(),
        false => format!("file {}\n", found.size),
    };
    let noted = downloads
        .join(download_name(target))
        .with_extension("metadata");
    let _ = std::fs::create_dir_all(downloads).and_then(|()| std::fs::write(noted, text));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_files_need_no_request() {
        let downloads = tempfile::tempdir().expect("a temporary directory");
        let (copied, found, folder) = ("https://a/copied", "https://a/found", "s3://b/folder");
        assert!(known(copied, downloads.path()).is_none());
        let copy = downloads.path().join(download_name(copied));
        std::fs::write(copy, "12345").expect("writes");
        let size = |found: Option<Metadata>| found.map(|found| (found.directory, found.size));
        assert_eq!(size(known(copied, downloads.path())), Some((false, 5)));
        let file = Metadata {
            directory: false,
            size: 42,
            version: None,
        };
        note(found, downloads.path(), &file);
        assert_eq!(size(known(found, downloads.path())), Some((false, 42)));
        let directory = Metadata {
            directory: true,
            size: 0,
            version: None,
        };
        note(folder, downloads.path(), &directory);
        assert_eq!(size(known(folder, downloads.path())), Some((true, 0)));
    }

    #[test]
    fn stopped_downloads_are_taken_over() {
        let downloads = tempfile::tempdir().expect("a temporary directory");
        let part = |name: &str, size: usize| {
            std::fs::write(downloads.path().join(name), vec![b'x'; size]).expect("writes");
        };
        // A part of this process, which runs, and parts of stopped runs.
        part(&format!("abc.{}.partial", std::process::id()), 9);
        part("abc.4294967295.partial", 5);
        part("abc.partial", 3);
        part("other.4294967295.partial", 7);
        let own = downloads.path().join("abc.1.partial");
        assert_eq!(take_over(downloads.path(), "abc", &own), 5);
        assert!(own.is_file());
        assert_eq!(take_over(downloads.path(), "abc", &own), 3);
        assert_eq!(take_over(downloads.path(), "abc", &own), 0);
    }

    #[test]
    fn copies_keep_the_names_of_earlier_runs() {
        // The SHA-256 of the public web address, as `iunctus/src/remote.rs` named copies.
        let name = |text: &str| -> String {
            Sha256::digest(text.as_bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect()
        };
        assert_eq!(
            download_name("s3://ngi-igenomes/a.fa"),
            name("https://ngi-igenomes.s3.amazonaws.com/a.fa")
        );
        assert_eq!(download_name("ftp://host/a"), name("ftp://host/a"));
    }
}
