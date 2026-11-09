//! Caches remote downloads as immutable bundles scoped to provider and address.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
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

/// The file that processes lock shared while they may read from a folder of downloads.
const IN_USE: &str = "in-use.lock";

/// Locks a folder of downloads alone, so that no process reads from it until the file goes;
/// `None` while one may, or when there is no such folder.
pub fn hold_downloads(downloads: &Path) -> std::io::Result<Option<std::fs::File>> {
    if !downloads.is_dir() {
        return Ok(None);
    }
    let file = lock_file(&downloads.join(IN_USE))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => Err(error),
    }
}

fn lock_file(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

async fn lock_download(path: &Path) -> Result<std::fs::File, StorageError> {
    let file = lock_file(path).map_err(local(path))?;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(local(path)(error)),
        }
    }
}

impl Storage {
    /// Locks a folder of downloads shared until this process ends, so that `uwe clean`
    /// removes nothing that it reads; waits while clean-up holds it alone.
    pub async fn hold(&self, downloads: &Path) -> Result<(), StorageError> {
        let known = |held: &HashMap<PathBuf, std::fs::File>| held.contains_key(downloads);
        if known(
            &self
                .inner
                .held
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()),
        ) {
            return Ok(());
        }
        std::fs::create_dir_all(downloads).map_err(local(downloads))?;
        let path = downloads.join(IN_USE);
        let file = lock_file(&path).map_err(local(&path))?;
        loop {
            match file.try_lock_shared() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) => {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Err(std::fs::TryLockError::Error(error)) => return Err(local(&path)(error)),
            }
        }
        let mut held = self
            .inner
            .held
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        held.insert(downloads.to_owned(), file);
        Ok(())
    }

    /// The ready-bundle name for this provider and address, excluding provider credentials.
    pub fn download_name(&self, target: &str) -> Result<String, StorageError> {
        self.download_identity(target).map(|(name, _)| name)
    }

    fn download_identity(&self, target: &str) -> Result<(String, Option<Metadata>), StorageError> {
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
        let observed = self
            .inner
            .observed
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let entry = observed.get(target).or_else(|| {
            observed
                .iter()
                .filter(|(query, entry)| entry.metadata.directory && address::serves(query, target))
                .max_by_key(|(query, _)| query.len())
                .map(|(_, entry)| entry)
        });
        if let Some(entry) = entry {
            identity.push(entry.stamp.clone());
        }
        let metadata = entry.map(|entry| entry.metadata.clone());
        drop(observed);
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
        Ok((format!("v2-{hex}"), metadata))
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

    /// Where the copy of the current version of a file is, as [`Storage::fetch`] names it. It
    /// asks the provider for the version once per process, but downloads nothing.
    pub async fn current_path(
        &self,
        target: &str,
        downloads: &Path,
    ) -> Result<PathBuf, StorageError> {
        let remote = address::scheme(target).is_some() && !target.starts_with("file://");
        if remote && self.inner.journal.pinned(target).is_none() {
            self.observe(target).await;
        }
        self.download_path(target, downloads)
    }

    /// A local copy of a file, downloaded into `downloads` unless an earlier call downloaded
    /// the same version. A local path, or a `file://` address, is its own copy.
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
        self.hold(downloads).await?;
        self.observe(target).await;
        let (name, expected) = self.download_identity(target)?;
        let bundle = downloads.join(name);
        let copy = bundle.join("content");
        // Tasks of a run that read the same file at once would write the same part.
        let turn = self.turn(&copy.to_string_lossy());
        let _turn = turn.lock().await;
        let _owner = lock_download(&bundle.with_extension("lock")).await?;
        if !tokio::fs::try_exists(&copy).await.map_err(local(&copy))? {
            tokio::fs::create_dir_all(downloads)
                .await
                .map_err(local(downloads))?;
            let (temporary, downloaded) = self.download_private(target, &bundle).await?;
            if let Some(expected) = expected.as_ref()
                && expected.version.is_some()
                && (expected.version != downloaded.version
                    || expected.size != temporary.path().metadata().map_err(local(&copy))?.len())
            {
                let served = self.serve(target)?;
                return Err(served.failed(
                    "read",
                    target,
                    crate::error::Problem::passing(
                        "the source changed after its cache identity was observed",
                    ),
                ));
            }
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
        if let Some(expected) = expected
            .as_ref()
            .and_then(|metadata| metadata.version.as_ref())
            && std::fs::read_to_string(copy.with_extension("version"))
                .ok()
                .as_ref()
                != Some(expected)
        {
            let served = self.serve(target)?;
            return Err(served.failed("read", target, crate::error::Problem::fixed("cached content belongs to another observed version; remove the stale download before retrying")));
        }
        self.inner
            .journal
            .note(journal::described(target, &copy).await?);
        Ok(copy)
    }

    /// The lock of the download of a copy, or of a request for a version, which all fetches
    /// of its file in this process share.
    pub(crate) fn turn(&self, name: &str) -> std::sync::Arc<tokio::sync::Mutex<()>> {
        let mut locks =
            (self.inner.downloading.lock()).unwrap_or_else(|poison| poison.into_inner());
        locks.entry(name.to_owned()).or_default().clone()
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
        // A later run that finds this copy notes its version from here, the one of these bytes.
        let noted = journal::with_suffix(file, "version");
        let _ = match &version {
            Some(version) => std::fs::write(&noted, version),
            None => std::fs::remove_file(&noted),
        };
        self.inner.journal.note(Fetched { version, ..fetched });
        Ok(())
    }

    /// Notes the copy at `file` that an earlier [`Storage::copy_of`] made as read, as that call
    /// notes it; a pinned copy of the address replaces it.
    pub async fn note_copy(&self, target: &str, file: &Path) -> Result<(), StorageError> {
        if self.inner.journal.pinned(target).is_some() {
            return self.copy_of(target, file).await;
        }
        let fetched = journal::described(target, file).await?;
        self.inner.journal.note(fetched);
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

    /// A name of the pins of the process, so that copies with pinned bytes can stay apart from
    /// the copies that other runs share; none without pins.
    pub fn pinned_name(&self) -> Option<String> {
        self.inner.journal.pins_name()
    }

    /// Whether a remote file exists, and its size. As [`Storage::fetch`] does, it asks the
    /// provider once per process; a copy or a note in `downloads` of that version answers
    /// later calls, and a file that a request finds is noted there.
    pub async fn metadata(
        &self,
        target: &str,
        downloads: &Path,
    ) -> Result<Option<Metadata>, StorageError> {
        if let Some(pinned) = self.inner.journal.pinned(target) {
            return Ok(Some(Metadata {
                directory: false,
                modified: None,
                size: pinned.size,
                version: pinned.version,
            }));
        }
        if !self.version_known(target) {
            let turn = self.turn(&format!("version of {target}"));
            let _turn = turn.lock().await;
            if !self.version_known(target) {
                let found = self.current(target).await?;
                if let Some(found) = &found {
                    note(&downloads.join(self.download_name(target)?), found);
                }
                return Ok(found);
            }
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
            modified: None,
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
            modified: None,
            size: size.parse().ok()?,
            version: None,
        }),
        None if noted.trim() == "directory" => Some(Metadata {
            directory: true,
            modified: None,
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
            modified: None,
            size: 42,
            version: None,
        };
        note(&bundle(found), &file);
        assert_eq!(size(known(&bundle(found))), Some((false, 42)));
        let directory = Metadata {
            directory: true,
            modified: None,
            size: 0,
            version: None,
        };
        note(&bundle(folder), &directory);
        assert_eq!(size(known(&bundle(folder))), Some((true, 0)));
    }

    #[test]
    fn preserves_historical_names() {
        // The SHA-256 of the public web address, as `uwe/src/remote.rs` named copies.
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
