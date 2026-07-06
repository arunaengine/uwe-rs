//! The connection of a provider: its client, made when the provider is first used, with its
//! secrets read then. Local `file://` addresses need no provider of the config file.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use crate::error::Problem;
use crate::folder::Folder;
use crate::settings::{Kind, ProviderSettings};
use crate::{Entry, Metadata, StorageError};

/// Where a provider comes from.
pub(crate) enum Source {
    /// A provider of the config file.
    Configured(Box<ProviderSettings>),
    /// `file` addresses.
    Files,
}

impl Source {
    /// The provider in words, for messages.
    pub fn describe(&self) -> String {
        match self {
            Source::Configured(settings) => format!("storage provider `{}`", settings.name),
            Source::Files => "the local file system".to_owned(),
        }
    }
}

/// The client of a provider.
pub(crate) enum Backend {
    Folder(Folder),
}

/// A provider's client and the secrets that its messages must not show.
pub(crate) struct Connection {
    pub backend: Backend,
    pub secrets: Vec<String>,
}

impl Connection {
    /// Makes the client of a provider.
    pub fn open(source: &Source) -> Result<Connection, StorageError> {
        let backend = match source {
            Source::Files => Backend::Folder(Folder {
                root: PathBuf::from("/"),
            }),
            Source::Configured(settings) => match &settings.kind {
                Kind::Folder(folder) => Backend::Folder(Folder {
                    root: folder.path.clone(),
                }),
                _ => {
                    return Err(StorageError::Unsupported(format!(
                        "iunctus cannot use the storage provider `{}` yet",
                        settings.name
                    )));
                }
            },
        };
        Ok(Connection {
            backend,
            secrets: Vec::new(),
        })
    }

    /// Whether the provider can list the entries of a directory.
    pub fn lists(&self) -> bool {
        matches!(self.backend, Backend::Folder(_))
    }

    /// Whether the provider can write files.
    pub fn writes(&self) -> bool {
        matches!(self.backend, Backend::Folder(_))
    }

    pub async fn stat(&self, prefix: &str, target: &str) -> Result<Option<Metadata>, Problem> {
        match &self.backend {
            Backend::Folder(folder) => folder.stat(&folder.path(prefix, target)?).await,
        }
    }

    pub async fn list(&self, prefix: &str, target: &str) -> Result<Vec<Entry>, Problem> {
        match &self.backend {
            Backend::Folder(folder) => folder.list(&folder.path(prefix, target)?).await,
        }
    }

    /// Downloads a file into `file`, from `offset` on where the provider can; returns whether
    /// it appended to what `file` held.
    pub async fn download(
        &self,
        prefix: &str,
        target: &str,
        file: &Path,
        _offset: u64,
    ) -> Result<bool, Problem> {
        match &self.backend {
            Backend::Folder(folder) => folder.download(&folder.path(prefix, target)?, file).await,
        }
    }

    pub async fn upload(&self, file: &Path, prefix: &str, target: &str) -> Result<(), Problem> {
        match &self.backend {
            Backend::Folder(folder) => folder.upload(file, &folder.path(prefix, target)?).await,
        }
    }
}
