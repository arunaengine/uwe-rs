//! Remote files downloaded once: a local copy lives in a folder of downloads under the SHA-256
//! of its provider and address. Content and its version are published as one immutable bundle.
//! What an earlier run found out about a remote file is noted there too.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::local;
use crate::journal::{self, Fetched};
use crate::{Metadata, Storage, StorageError, address};

/// The legacy address-only name, retained for compatibility. New caches use
/// [`Storage::download_name`] and never trust unverifiable legacy copies.
pub fn download_name(target: &str) -> String {
    let named = address::public_url(target).unwrap_or_else(|| target.to_owned());
    Sha256::digest(named.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl Storage {
    /// The ready-bundle name for this provider and address, excluding provider credentials.
    pub fn download_name(&self, target: &str) -> Result<String, StorageError> {
        let (provider, prefix) = crate::longest(&self.inner.configured, target)
            .or_else(|| crate::longest(&self.inner.built_in, target))
            .ok_or_else(|| StorageError::NoProvider {
                address: address::redacted(target),
                prefix: crate::suggested_prefix(target),
            })?;
        let mut identity = provider.source.cache_identity();
        identity.push(address::redacted(prefix));
        let routed = match &provider.source {
            crate::backend::Source::Web | crate::backend::Source::PublicBuckets => {
                address::public_url(target).unwrap_or_else(|| target.into())
            }
            _ => target.into(),
        };
        identity.push(address::redacted(&routed));
        let mut hasher = Sha256::new();
        for field in identity {
            hasher.update((field.len() as u64).to_le_bytes());
            hasher.update(field.as_bytes());
        }
        let hex: String = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(format!("v2-{hex}"))
    }

    /// Where a fetched file lives; calculating it performs no I/O or credential lookup.
    pub fn download_path(&self, target: &str, downloads: &Path) -> Result<PathBuf, StorageError> {
        if address::scheme(target).is_none() {
            return Ok(target.into());
        }
        if let Some(path) = target.strip_prefix("file://") {
            return Ok(address::decode(path).into());
        }
        if let Some(pinned) = self.inner.journal.pinned(target) {
            return Ok(pinned.copy);
        }
        Ok(downloads.join(self.download_name(target)?).join("content"))
    }

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
        let bundle = downloads.join(self.download_name(target)?);
        let copy = bundle.join("content");
        // Tasks of a run that read the same file at once would write the same part.
        let turn = self.turn(&copy);
        let _turn = turn.lock().await;
        if !tokio::fs::try_exists(&copy).await.map_err(local(&copy))? {
            tokio::fs::create_dir_all(downloads)
                .await
                .map_err(local(downloads))?;
            let (temporary, downloaded) = self.download_private(target, &bundle).await?;
            // Create the bundle only after transfer I/O, so queued work cannot outlive it.
            let ready = tempfile::tempdir_in(downloads).map_err(local(downloads))?;
            let content = ready.path().join("content");
            temporary
                .persist(&content)
                .map_err(|error| local(&content)(error.error))?;
            if let Some(version) = downloaded.version {
                std::fs::write(content.with_extension("version"), version)
                    .map_err(local(&content))?;
            }
            // A complete winner is nonempty, so another directory rename cannot replace it.
            if let Err(error) = std::fs::rename(ready.path(), &bundle)
                && (!matches!(
                    error.kind(),
                    std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::DirectoryNotEmpty
                ) || !copy.is_file())
            {
                return Err(local(&bundle)(error));
            }
        }
        self.inner
            .journal
            .note(journal::described(target, &copy).await?);
        Ok(copy)
    }

    /// The lock of the download of a copy, which all fetches of its file in this process
    /// share.
    fn turn(&self, copy: &Path) -> std::sync::Arc<tokio::sync::Mutex<()>> {
        let mut locks =
            (self.inner.downloading.lock()).unwrap_or_else(|poison| poison.into_inner());
        locks
            .entry(copy.to_string_lossy().into_owned())
            .or_default()
            .clone()
    }

    /// Downloads a file into `file`, whole, as a transfer: the process does not note it as a
    /// file that it read, like a result that goes from a store elsewhere to its place.
    pub async fn download(&self, target: &str, file: &Path) -> Result<(), StorageError> {
        self.download_from(target, file).await.map(|_| ())
    }

    /// A copy of a remote file at `file`, which the process notes as read, like a file of a
    /// workflow on the web that a run keeps next to the others; a pinned copy answers for its
    /// address.
    pub async fn copy_of(&self, target: &str, file: &Path) -> Result<(), StorageError> {
        let (fetched, version) = match self.inner.journal.pinned(target) {
            Some(pinned) => {
                let parent = file
                    .parent()
                    .filter(|path| !path.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new("."));
                let temporary = tempfile::NamedTempFile::new_in(parent).map_err(local(file))?;
                let temporary = crate::folder::copied(&pinned.copy, temporary)
                    .await
                    .map_err(local(file))?;
                temporary
                    .persist(file)
                    .map_err(|error| local(file)(error.error))?;
                let version = pinned.version.clone();
                (pinned, version)
            }
            None => {
                let downloaded = self.download_from(target, file).await?;
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
        let bundle = downloads.join(self.download_name(target)?);
        if let Some(known) = known(&bundle) {
            return Ok(Some(known));
        }
        let found = self.stat(target).await?;
        if let Some(found) = &found {
            note(&bundle, found);
        }
        Ok(found)
    }
}

/// What `downloads` knows of a remote file: the size of its copy, or what an earlier run noted.
fn known(bundle: &Path) -> Option<Metadata> {
    let copy = bundle.join("content");
    if let Some(found) = std::fs::metadata(&copy)
        .ok()
        .filter(|found| found.is_file())
    {
        return Some(Metadata {
            directory: false,
            size: found.len(),
            version: std::fs::read_to_string(copy.with_extension("version"))
                .ok()
                .filter(|version| !version.is_empty()),
        });
    }
    let noted = std::fs::read_to_string(bundle.with_extension("metadata")).ok()?;
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
fn note(bundle: &Path, found: &Metadata) {
    let text = match found.directory {
        true => "directory\n".to_owned(),
        false => format!("file {}\n", found.size),
    };
    let noted = bundle.with_extension("metadata");
    let _ = std::fs::create_dir_all(bundle.parent().unwrap_or_else(|| Path::new(".")))
        .and_then(|()| std::fs::write(noted, text));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_avoids_requests() {
        let downloads = tempfile::tempdir().expect("a temporary directory");
        let (copied, found, folder) = ("https://a/copied", "https://a/found", "s3://b/folder");
        let storage = Storage::default();
        let bundle = |target| {
            downloads
                .path()
                .join(storage.download_name(target).unwrap())
        };
        assert!(known(&bundle(copied)).is_none());
        let copy = bundle(copied).join("content");
        std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
        std::fs::write(copy, "12345").expect("writes");
        let size = |found: Option<Metadata>| found.map(|found| (found.directory, found.size));
        assert_eq!(size(known(&bundle(copied))), Some((false, 5)));
        let file = Metadata {
            directory: false,
            size: 42,
            version: None,
        };
        note(&bundle(found), &file);
        assert_eq!(size(known(&bundle(found))), Some((false, 42)));
        let directory = Metadata {
            directory: true,
            size: 0,
            version: None,
        };
        note(&bundle(folder), &directory);
        assert_eq!(size(known(&bundle(folder))), Some((true, 0)));
    }

    #[test]
    fn preserves_historical_names() {
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
