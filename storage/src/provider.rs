//! Lets an embedding host serve address schemes of its own, like `aruna://`, with digests.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use futures_util::future::BoxFuture;

use crate::{Entry, Metadata, StorageError, UploadReceipt, address};

/// Where a worker on another machine reads the bytes of a file itself, so that they never pass
/// through this machine.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct DirectRef {
    /// A URL that the worker reads, like a presigned one, or an address that its stager knows.
    pub url: String,
    /// Headers that the worker sends with the request, like an authorization; they are secrets.
    pub headers: Vec<(String, String)>,
    /// The id of the object in Aruna, for stagers that read Aruna objects natively.
    pub aruna_id: Option<String>,
    /// The endpoint that holds the bytes nearest to the workers, like an Aruna DataProxy.
    pub endpoint: Option<String>,
    /// How the bytes are encrypted at rest, for stagers that decrypt them at the edge.
    pub encryption: Option<Encryption>,
}

/// Shows where the reference points without its secrets: no query and no header values.
impl fmt::Debug for DirectRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let url = self.url.split('?').next().unwrap_or_default();
        let headers: Vec<&str> = self.headers.iter().map(|(name, _)| name.as_str()).collect();
        formatter
            .debug_struct("DirectRef")
            .field("url", &address::redacted(url))
            .field("headers", &headers)
            .field("aruna_id", &self.aruna_id)
            .field("endpoint", &self.endpoint)
            .field("encryption", &self.encryption)
            .finish()
    }
}

/// How the bytes of a direct reference are encrypted at rest, like with Crypt4GH.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Encryption {
    /// The scheme, like `crypt4gh`.
    pub scheme: String,
    /// What a stager needs besides its own keys, like the id of the key of a recipient.
    pub parameters: BTreeMap<String, String>,
}

/// What a provider says of a file: its metadata, and where it knows them the verified digest of
/// its content, the endpoints that hold it and a direct reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Details {
    pub metadata: Metadata,
    /// The SHA-256 of the content as 64 lowercase hexadecimal digits, which the provider
    /// verified and which stays the same for this address.
    pub digest: Option<String>,
    /// The endpoints that hold the bytes, the nearest first.
    pub endpoints: Vec<String>,
    pub direct: Option<DirectRef>,
}

impl Details {
    /// The details of a provider that knows only the metadata.
    pub fn of(metadata: Metadata) -> Details {
        Details {
            metadata,
            digest: None,
            endpoints: Vec::new(),
            direct: None,
        }
    }

    /// The digest, when it is a well-formed SHA-256 of a file.
    pub fn verified(&self) -> Option<&str> {
        self.digest
            .as_deref()
            .filter(|_| !self.metadata.directory)
            .filter(|digest| sha256(digest))
    }
}

/// Whether a text is a SHA-256 as 64 lowercase hexadecimal digits.
pub fn sha256(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// One file of a remote directory, by its path in the directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeFile {
    /// The path below the directory, with `/` between its parts.
    pub path: String,
    pub size: u64,
    /// The verified SHA-256 of the content, as in [`Details::digest`].
    pub digest: Option<String>,
    pub executable: bool,
    pub direct: Option<DirectRef>,
}

/// What a remote directory holds: its files, and its empty directories by their paths in it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeManifest {
    pub files: Vec<TreeFile>,
    pub empty: Vec<String>,
}

impl TreeManifest {
    /// Whether every file has a verified digest and a direct reference, so that a worker can
    /// fetch the whole directory itself.
    pub fn direct(&self) -> bool {
        (self.files.iter())
            .all(|file| file.direct.is_some() && file.digest.as_deref().is_some_and(sha256))
    }
}

/// A storage provider of an embedding host, which [`crate::Storage::with_provider`] puts ahead
/// of the providers of the config file and the built-in ones.
pub trait StorageProvider: Send + Sync + 'static {
    /// The name of the provider in messages, like `aruna`.
    fn name(&self) -> &str;

    /// Whether the provider serves an address, like any that starts with `aruna://`.
    fn matches(&self, address: &str) -> bool;

    /// What tells the files of this provider apart from those of another provider of the same
    /// name at the same address, like the tenant it reads for, in a shared folder of downloads.
    /// `None` keeps its downloads to this instance.
    fn cache_scope(&self) -> Option<String> {
        None
    }

    /// What the provider knows of a file or directory; `None` when it is not there.
    fn stat<'a>(&'a self, address: &'a str)
    -> BoxFuture<'a, Result<Option<Details>, StorageError>>;

    /// The entries of a directory.
    fn list<'a>(&'a self, address: &'a str) -> BoxFuture<'a, Result<Vec<Entry>, StorageError>> {
        Box::pin(async move { Err(unsupported(self.name(), "list", address)) })
    }

    /// Every file and empty directory below a directory. By default it walks [`Self::list`]
    /// and asks [`Self::stat`] for each file.
    fn tree_manifest<'a>(
        &'a self,
        address: &'a str,
    ) -> BoxFuture<'a, Result<TreeManifest, StorageError>> {
        Box::pin(walk(self, address))
    }

    /// Downloads a file into `target`, which exists and is empty, and gives the version of
    /// the bytes as `stat` reports it, else `None`.
    fn download<'a>(
        &'a self,
        address: &'a str,
        target: &'a Path,
    ) -> BoxFuture<'a, Result<Option<String>, StorageError>>;

    /// Uploads a local file.
    fn upload<'a>(
        &'a self,
        source: &'a Path,
        address: &'a str,
    ) -> BoxFuture<'a, Result<UploadReceipt, StorageError>> {
        let _ = source;
        Box::pin(async move { Err(unsupported(self.name(), "write", address)) })
    }

    /// Uploads a local file unless the address holds content already; `None` then.
    fn upload_absent<'a>(
        &'a self,
        source: &'a Path,
        address: &'a str,
    ) -> BoxFuture<'a, Result<Option<UploadReceipt>, StorageError>> {
        let _ = source;
        Box::pin(async move { Err(unsupported(self.name(), "write", address)) })
    }

    /// A reference that a worker reads the file with, for staging without this machine. By
    /// default it is the one of [`Self::stat`].
    fn direct_ref<'a>(
        &'a self,
        address: &'a str,
    ) -> BoxFuture<'a, Result<Option<DirectRef>, StorageError>> {
        Box::pin(async move { Ok(self.stat(address).await?.and_then(|details| details.direct)) })
    }
}

/// The files and empty directories below `root`, found with `list` and `stat`.
async fn walk<P: StorageProvider + ?Sized>(
    provider: &P,
    root: &str,
) -> Result<TreeManifest, StorageError> {
    let base = root.trim_end_matches('/');
    let mut manifest = TreeManifest::default();
    let mut pending = vec![String::new()];
    while let Some(relative) = pending.pop() {
        let folder = joined(base, &relative);
        let entries = provider.list(&format!("{folder}/")).await?;
        if entries.is_empty() && !relative.is_empty() {
            manifest.empty.push(relative.clone());
        }
        for entry in entries {
            let name = entry.name.trim_end_matches('/');
            // A name must stay one name in the folder, before any request goes out with it.
            if name.is_empty()
                || name == "."
                || name == ".."
                || name.contains(['/', '\\'])
                || name.chars().any(char::is_control)
            {
                return Err(StorageError::Failed {
                    action: "list",
                    address: address::redacted(&folder),
                    provider: provider.name().to_owned(),
                    message: format!("the listing names an invalid entry {:?}", entry.name),
                    retryable: false,
                });
            }
            let path = joined(&relative, name).trim_start_matches('/').to_owned();
            if entry.directory {
                pending.push(path);
                continue;
            }
            let details = provider.stat(&joined(base, &path)).await?;
            let details = details.ok_or_else(|| StorageError::Missing {
                address: address::redacted(&joined(base, &path)),
                provider: provider.name().to_owned(),
            })?;
            manifest.files.push(TreeFile {
                path,
                size: details.metadata.size,
                executable: false,
                digest: details.verified().map(str::to_owned),
                direct: details.direct,
            });
        }
    }
    manifest
        .files
        .sort_by(|first, second| first.path.cmp(&second.path));
    manifest.empty.sort();
    Ok(manifest)
}

/// `relative` below `base`, or `base` itself for an empty `relative`.
pub(crate) fn joined(base: &str, relative: &str) -> String {
    match relative.is_empty() {
        true => base.to_owned(),
        false => format!("{base}/{relative}"),
    }
}

fn unsupported(name: &str, action: &str, target: &str) -> StorageError {
    StorageError::Unsupported(format!(
        "cannot {action} {}: the storage provider `{name}` cannot do that",
        address::redacted(target)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lists one entry of the given name, and counts the files it is asked about.
    struct Listing(&'static str, std::sync::Mutex<usize>);

    impl StorageProvider for Listing {
        fn name(&self) -> &str {
            "listing"
        }

        fn matches(&self, _address: &str) -> bool {
            true
        }

        fn stat<'a>(
            &'a self,
            _address: &'a str,
        ) -> BoxFuture<'a, Result<Option<Details>, StorageError>> {
            *self.1.lock().unwrap() += 1;
            Box::pin(async { Ok(None) })
        }

        fn list<'a>(
            &'a self,
            _address: &'a str,
        ) -> BoxFuture<'a, Result<Vec<Entry>, StorageError>> {
            let name = self.0.to_owned();
            Box::pin(async move {
                Ok(vec![Entry {
                    name,
                    directory: false,
                }])
            })
        }

        fn download<'a>(
            &'a self,
            _address: &'a str,
            _target: &'a Path,
        ) -> BoxFuture<'a, Result<Option<String>, StorageError>> {
            Box::pin(async { Ok(None) })
        }
    }

    #[tokio::test]
    async fn refuses_listed_names() {
        for name in ["..", ".", "", "a/b", "a\nb", "a\\b"] {
            let listing = Listing(name, std::sync::Mutex::new(0));
            let walked = walk(&listing, "listing://root").await;
            assert!(walked.is_err(), "{name:?}");
            assert_eq!(*listing.1.lock().unwrap(), 0, "no request names {name:?}");
        }
    }
}
