//! The remote files that a process read, and copies that answer for addresses. A run records
//! what it read by digest, so that a later run can tell whether a remote file changed; a repeat
//! of a run pins each address to the copy it read, so that it reads the same bytes again.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::StorageError;
use crate::address::redacted;
use crate::error::local;

/// A remote file as the process read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fetched {
    /// The address, with a password that it holds hidden.
    pub address: String,
    /// The local copy that the process read.
    pub copy: PathBuf,
    /// `sha256:<hex>` of the content.
    pub digest: String,
    pub size: u64,
    /// When the content was read from the address; an earlier process may have read it.
    pub time: SystemTime,
    /// The version that the provider gave then, like an ETag, when known.
    pub version: Option<String>,
}

/// What the providers of a process read, and the copies that answer for addresses.
#[derive(Default)]
pub(crate) struct Journal {
    /// Each address once, in the order it was first read.
    read: Mutex<Vec<Fetched>>,
    pinned: Mutex<HashMap<String, Fetched>>,
}

impl Journal {
    /// Notes that the process read a file; a later read of the same address replaces it.
    pub(crate) fn note(&self, fetched: Fetched) {
        let mut read = self.read.lock().unwrap_or_else(PoisonError::into_inner);
        match read
            .iter_mut()
            .find(|known| known.address == fetched.address)
        {
            Some(known) => *known = fetched,
            None => read.push(fetched),
        }
    }

    pub(crate) fn read(&self) -> Vec<Fetched> {
        self.read
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn pin(&self, fetched: Fetched) {
        let mut pinned = self.pinned.lock().unwrap_or_else(PoisonError::into_inner);
        pinned.insert(redacted(&fetched.address), fetched);
    }

    /// The copy that answers for an address, when one does. Addresses are compared without
    /// the passwords they hold, as runs record them.
    pub(crate) fn pinned(&self, address: &str) -> Option<Fetched> {
        let pinned = self.pinned.lock().unwrap_or_else(PoisonError::into_inner);
        pinned.get(&redacted(address)).cloned()
    }
}

/// What the process read at an address from a local copy: its digest and size, remembered
/// next to the copy while the copy does not change, and the version that a note next to the
/// copy holds. The time is when the copy was written. A password in the address is hidden, so
/// that no record of the read holds it.
pub(crate) async fn described(address: &str, copy: &Path) -> Result<Fetched, StorageError> {
    let (address, copy) = (redacted(address), copy.to_owned());
    let described = tokio::task::spawn_blocking(move || {
        let found = std::fs::metadata(&copy).map_err(local(&copy))?;
        let time = found.modified().unwrap_or(UNIX_EPOCH);
        let stamp = format!("{} {}", found.len(), nanoseconds(time));
        let note = with_suffix(&copy, "sha256");
        let digest = match std::fs::read_to_string(&note) {
            Ok(text) => text
                .split_once(' ')
                .filter(|(_, noted)| *noted == stamp)
                .map(|(digest, _)| digest.to_owned()),
            Err(_) => None,
        };
        let digest = match digest {
            Some(digest) => digest,
            None => {
                let digest = hash(&copy).map_err(local(&copy))?;
                // A note that cannot be written only costs hashing the copy again.
                let _ = std::fs::write(&note, format!("{digest} {stamp}"));
                digest
            }
        };
        let version = std::fs::read_to_string(with_suffix(&copy, "version")).ok();
        Ok(Fetched {
            address,
            size: found.len(),
            time,
            version: version.filter(|text| !text.is_empty()),
            digest,
            copy,
        })
    });
    described.await.map_err(|error| {
        StorageError::Unsupported(format!("the digest of a download stopped: {error}"))
    })?
}

/// Notes the version of the content of a copy next to it, for later runs that read the copy.
#[cfg(test)]
pub(crate) fn note_version(copy: &Path, version: Option<&str>) {
    if let Some(version) = version {
        let _ = std::fs::write(with_suffix(copy, "version"), version);
    }
}

/// `sha256:<hex>` of a local file, read in parts.
fn hash(file: &Path) -> std::io::Result<String> {
    let mut reader = std::fs::File::open(file)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 16];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let hex: String = (hasher.finalize().iter())
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(format!("sha256:{hex}"))
}

/// The path of a note next to a file: its name with `.<suffix>` after it.
fn with_suffix(file: &Path, suffix: &str) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(format!(".{suffix}"));
    PathBuf::from(name)
}

fn nanoseconds(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn remembers_unchanged_digests() {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let copy = folder.path().join("copy");
        std::fs::write(&copy, "abc").expect("writes");
        note_version(&copy, Some("\"v1\""));
        let first = described("https://x/a", &copy).await.expect("describes");
        let expected = "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(first.digest, expected);
        assert_eq!(first.size, 3);
        assert_eq!(first.version.as_deref(), Some("\"v1\""));
        let note = with_suffix(&copy, "sha256");
        assert!(
            std::fs::read_to_string(&note)
                .expect("a note")
                .starts_with(expected)
        );
        // A note that no longer fits the copy is not taken.
        std::fs::write(&copy, "abcd").expect("writes");
        let second = described("https://x/a", &copy).await.expect("describes");
        assert_ne!(second.digest, expected);
        assert_eq!(second.size, 4);
    }

    #[tokio::test]
    async fn omits_journal_passwords() {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let copy = folder.path().join("copy");
        std::fs::write(&copy, "abc").expect("writes");
        let noted = described("ftp://me:pass@host/a.txt", &copy)
            .await
            .expect("describes");
        assert_eq!(noted.address, "ftp://me:[secret]@host/a.txt");
        let journal = Journal::default();
        journal.pin(noted);
        assert!(journal.pinned("ftp://me:pass@host/a.txt").is_some());
        assert!(journal.pinned("ftp://host/a.txt").is_none());
    }

    #[test]
    fn journals_unique_addresses() {
        let journal = Journal::default();
        let fetched = |digest: &str| Fetched {
            address: "https://x/a".into(),
            copy: PathBuf::from("/c"),
            digest: digest.into(),
            size: 1,
            time: UNIX_EPOCH,
            version: None,
        };
        journal.note(fetched("sha256:1"));
        journal.note(fetched("sha256:2"));
        let read = journal.read();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].digest, "sha256:2");
        assert!(journal.pinned("https://x/a").is_none());
        journal.pin(fetched("sha256:1"));
        assert_eq!(
            journal.pinned("https://x/a").map(|pinned| pinned.digest),
            Some("sha256:1".into())
        );
    }
}
