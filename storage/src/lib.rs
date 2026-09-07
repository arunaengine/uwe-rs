//! Storage providers (decision 20). The config file maps address prefixes to providers, each
//! with its endpoint, region, credentials and settings: S3 and the services that speak its API,
//! Google Cloud Storage, Azure Blob Storage, web servers, FTP servers and local folders. The
//! longest prefix that serves an address wins. Web addresses, public buckets, FTP servers and
//! local files that no provider serves go to a built-in one, so that a machine without a config file reads
//! public files as before. Each provider makes its client once per process, when it is first
//! used; secrets never show in messages. The storage notes each remote file that it reads, by
//! digest, so that runs can record them, and a repeat of a run can pin addresses to the copies
//! that the run read.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod address;
mod backend;
mod blocking;
mod downloads;
mod error;
mod folder;
mod ftp;
mod journal;
mod objects;
mod public;
pub mod settings;
mod web;

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use config_file::{ConfigError, ConfigFile};

use backend::{Backend, Connection, Source};
use error::Problem;

pub use blocking::Blocking;
pub use downloads::download_name;
pub use error::StorageError;
pub use journal::Fetched;
pub use settings::Settings;
pub use web::{config_proxy, http_client, use_proxy};

/// Whether a file exists as a file or a directory, its size, and its version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Metadata {
    pub directory: bool,
    /// The size of a file; 0 for a directory.
    pub size: u64,
    /// What the provider says changes with the content of a file: its ETag, else the time of
    /// its last change; `None` when the provider tells neither.
    pub version: Option<String>,
}

/// What a download learned: the version of the file that the provider gave with it.
pub(crate) struct Downloaded {
    pub version: Option<String>,
}

/// An entry of a directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub directory: bool,
}

/// Where a cache store lives.
pub enum StoreLocation {
    /// A folder on this machine.
    Folder(PathBuf),
    /// A place in an object store, as an operator whose paths start there.
    Objects(opendal::Operator),
}

/// The storage providers of a process. Clones share the providers and their clients.
#[derive(Clone)]
pub struct Storage {
    inner: Arc<Inner>,
}

struct Inner {
    /// The providers of the config file.
    configured: Vec<Provider>,
    /// The providers of what the config file does not serve.
    built_in: Vec<Provider>,
    store: Option<String>,
    /// A folder of this machine in front of the store (`[store] local`).
    local_store: Option<String>,
    /// What the providers read, and the copies that answer for addresses.
    journal: journal::Journal,
    /// A lock for each copy in the folder of downloads, so that fetches of one file in this
    /// process take turns: the first downloads it, the others find its copy.
    downloading: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

/// A provider, and its client once it is used.
struct Provider {
    source: Source,
    prefixes: Vec<String>,
    connection: Mutex<Option<Arc<Connection>>>,
}

impl Provider {
    fn new(source: Source, prefixes: Vec<String>) -> Provider {
        Provider {
            source,
            prefixes,
            connection: Mutex::new(None),
        }
    }

    /// The client of the provider, made at the first use.
    fn connect(&self) -> Result<Arc<Connection>, StorageError> {
        let mut connection = self
            .connection
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(connection) = &*connection {
            return Ok(connection.clone());
        }
        let made = Arc::new(Connection::open(&self.source)?);
        *connection = Some(made.clone());
        Ok(made)
    }
}

/// A provider that serves an address, by the prefix that serves it.
struct Served<'a> {
    provider: &'a Provider,
    prefix: &'a str,
    connection: Arc<Connection>,
}

impl Served<'_> {
    /// An error of the provider, in words that hold none of its secrets.
    fn failed(&self, action: &'static str, address: &str, problem: Problem) -> StorageError {
        if problem.missing {
            return StorageError::Missing {
                address: address::redacted(address),
                provider: self.provider.source.describe(),
            };
        }
        StorageError::Failed {
            action,
            address: address::redacted(address),
            provider: self.provider.source.describe(),
            message: error::redact(&problem.message, &self.connection.secrets),
            retryable: problem.retryable,
        }
    }
}

/// Shows the providers by name and prefix, never their settings, which may hold secrets.
impl fmt::Debug for Storage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let providers: Vec<(String, &[String])> = (self.inner.configured.iter())
            .map(|provider| (provider.source.describe(), provider.prefixes.as_slice()))
            .collect();
        formatter
            .debug_struct("Storage")
            .field("providers", &providers)
            .field("store", &self.inner.store)
            .finish()
    }
}

impl Default for Storage {
    fn default() -> Self {
        Storage::new(Settings::default())
    }
}

impl Storage {
    pub fn new(settings: Settings) -> Storage {
        let configured = settings
            .providers
            .into_iter()
            .map(|provider| {
                let prefixes = provider.prefixes.clone();
                Provider::new(Source::Configured(Box::new(provider)), prefixes)
            })
            .collect();
        let prefixes =
            |prefixes: &[&str]| prefixes.iter().map(|&prefix| prefix.to_owned()).collect();
        let built_in = vec![
            Provider::new(Source::Web, prefixes(&["http://", "https://"])),
            Provider::new(Source::PublicBuckets, prefixes(&["s3://", "gs://"])),
            Provider::new(Source::Files, prefixes(&["file://"])),
            Provider::new(Source::Ftp, prefixes(&["ftp://"])),
        ];
        Storage {
            inner: Arc::new(Inner {
                configured,
                built_in,
                store: settings.store,
                local_store: settings.local_store,
                journal: journal::Journal::default(),
                downloading: Mutex::default(),
            }),
        }
    }

    /// The providers and the store of the config file.
    pub fn from_config(config: &ConfigFile) -> Result<Storage, ConfigError> {
        Ok(Storage::new(Settings::from_config(config)?))
    }

    /// Where the config file puts the cache store: a folder or an address.
    pub fn store_location(&self) -> Option<&str> {
        self.inner.store.as_deref()
    }

    /// The folder of this machine that the config file puts in front of the store, so that
    /// the store has two layers.
    pub fn local_store(&self) -> Option<&str> {
        self.inner.local_store.as_deref()
    }

    /// The provider that serves an address: the one with the longest prefix that serves it,
    /// else the built-in one.
    fn serve(&self, target: &str) -> Result<Served<'_>, StorageError> {
        let (provider, prefix) = longest(&self.inner.configured, target)
            .or_else(|| longest(&self.inner.built_in, target))
            .ok_or_else(|| StorageError::NoProvider {
                address: address::redacted(target),
                prefix: suggested_prefix(target),
            })?;
        Ok(Served {
            provider,
            prefix,
            connection: provider.connect()?,
        })
    }

    /// Whether a file exists, as a file or a directory, and its size.
    pub async fn stat(&self, target: &str) -> Result<Option<Metadata>, StorageError> {
        let served = self.serve(target)?;
        let found = served.connection.stat(served.prefix, target).await;
        found.map_err(|problem| served.failed("find", target, problem))
    }

    /// The entries of a directory.
    pub async fn list(&self, target: &str) -> Result<Vec<Entry>, StorageError> {
        let served = self.serve(target)?;
        if !served.connection.lists() {
            let message = format!(
                "cannot list {target}: {} cannot list the files of a folder",
                served.provider.source.describe()
            );
            return Err(StorageError::Unsupported(message));
        }
        let found = served.connection.list(served.prefix, target).await;
        found.map_err(|problem| served.failed("list", target, problem))
    }

    /// Downloads a whole file privately, then publishes it after a successful transfer.
    pub(crate) async fn download_from(
        &self,
        target: &str,
        file: &Path,
    ) -> Result<Downloaded, StorageError> {
        let served = self.serve(target)?;
        let parent = file
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let temporary = tempfile::NamedTempFile::new_in(parent).map_err(error::local(file))?;
        let done = (served.connection)
            .download(served.prefix, target, temporary.path(), 0)
            .await;
        let downloaded = done.map_err(|problem| served.failed("read", target, problem))?;
        temporary
            .persist(file)
            .map_err(|error| error::local(file)(error.error))?;
        Ok(downloaded)
    }

    /// Uploads a local file to an address.
    pub async fn upload(&self, file: &Path, target: &str) -> Result<(), StorageError> {
        let served = self.serve(target)?;
        if !served.connection.writes() {
            let message = format!(
                "cannot write {target}: {} only reads",
                served.provider.source.describe()
            );
            return Err(StorageError::Unsupported(message));
        }
        let done = served.connection.upload(file, served.prefix, target).await;
        done.map_err(|problem| served.failed("write", target, problem))
    }

    /// Where a cache store at `location` lives: a local path, a `file://` address, or an
    /// address in an object store or a folder provider.
    pub fn store(&self, location: &str) -> Result<StoreLocation, StorageError> {
        if address::scheme(location).is_none() {
            return Ok(StoreLocation::Folder(PathBuf::from(location)));
        }
        let served = self.serve(location)?;
        match &served.connection.backend {
            Backend::Folder(folder) => folder
                .path(served.prefix, location)
                .map(StoreLocation::Folder)
                .map_err(|problem| served.failed("keep the store at", location, problem)),
            Backend::Objects(objects) => objects
                .at(location)
                .map(StoreLocation::Objects)
                .map_err(|problem| served.failed("keep the store at", location, problem)),
            _ => Err(StorageError::Unsupported(format!(
                "cannot keep the store at {location}: {} only reads; name a storage provider \
                 of the config file that can write there",
                served.provider.source.describe()
            ))),
        }
    }

    /// The same requests for code that does not run on Tokio, like drivers.
    pub fn blocking(&self) -> Blocking {
        Blocking::new(self.clone())
    }
}

/// The provider whose prefix serves an address and is the longest, with that prefix.
fn longest<'a>(providers: &'a [Provider], target: &str) -> Option<(&'a Provider, &'a str)> {
    providers
        .iter()
        .flat_map(|provider| {
            (provider.prefixes.iter()).map(move |prefix| (provider, prefix.as_str()))
        })
        .filter(|(_, prefix)| address::serves(prefix, target))
        .max_by_key(|(_, prefix)| prefix.len())
}

/// A prefix that a provider for an address could serve, for a hint: its scheme and the first
/// part after it, like `az://container/`.
fn suggested_prefix(target: &str) -> String {
    match target.split_once("://") {
        Some((scheme, rest)) => {
            let first = rest.split('/').next().unwrap_or_default();
            format!("{scheme}://{first}/")
        }
        None => target.to_owned(),
    }
}
