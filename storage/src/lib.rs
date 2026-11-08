//! Routes storage addresses to configured providers and shared process clients.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod address;
pub mod aws;
mod azure;
mod backend;
mod blocking;
pub mod cas;
pub mod cbor;
mod downloads;
pub mod drs;
mod error;
mod folder;
mod ftp;
mod guard;
pub mod irods;
mod irods_protocol;
mod journal;
pub mod lakefs;
pub mod lamin;
mod objects;
pub mod oci;
pub mod oci_key;
mod public;
pub mod quilt;
pub mod quilt_package;
pub mod settings;
pub mod synapse;
mod web;

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use config_file::{ConfigError, ConfigFile};

use backend::{Backend, Connection, Source};
use error::Problem;

pub use blocking::{Answer, Blocking, exchange};
pub use downloads::{download_name, hold_downloads};
pub use error::StorageError;
pub use journal::Fetched;
pub use settings::Settings;
pub use web::{config_proxy, http_client, origin_client, trust_mozilla_roots, use_proxy};

/// Whether a file exists as a file or a directory, its size, and its version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Metadata {
    pub directory: bool,
    /// Last modification time, retaining provider precision when supplied.
    pub modified: Option<std::time::SystemTime>,
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

/// What a completed upload wrote, from the same write operation that committed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadReceipt {
    pub address: String,
    pub size: u64,
    /// The immutable provider version returned by the write, when the provider has one.
    pub version: Option<String>,
}

pub(crate) struct Written {
    pub size: u64,
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

/// Storage providers and read state. Clones share both within a session.
#[derive(Clone)]
pub struct Storage {
    inner: Arc<Inner>,
}

struct Inner {
    /// The providers of the config file.
    configured: Arc<Vec<Provider>>,
    /// The providers of what the config file does not serve.
    built_in: Arc<Vec<Provider>>,
    store: Option<String>,
    /// A folder of this machine in front of the store (`[store] local`).
    local_store: Option<String>,
    /// What the providers read, and the copies that answer for addresses.
    journal: journal::Journal,
    /// A lock for each copy in the folder of downloads, so that fetches of one file in this
    /// process take turns: the first downloads it, the others find its copy.
    downloading: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    observed: Mutex<HashMap<String, Observed>>,
    /// The folders of downloads that this process reads, each locked shared until it ends.
    held: Mutex<HashMap<PathBuf, std::fs::File>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        let held = self
            .held
            .get_mut()
            .unwrap_or_else(|poison| poison.into_inner());
        for file in held.values() {
            let _ = file.unlock();
        }
    }
}

struct Observed {
    metadata: Metadata,
    stamp: String,
}

/// A provider, and its client once it is used.
struct Provider {
    source: Source,
    prefixes: Vec<String>,
    connection: Mutex<Option<Arc<Connection>>>,
    /// The rules of a confined run for a built-in provider, which reaches only public hosts.
    guard: Option<guard::Guard>,
}

impl Provider {
    fn new(source: Source, prefixes: Vec<String>) -> Provider {
        Provider {
            source,
            prefixes,
            connection: Mutex::new(None),
            guard: None,
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
        let made = Arc::new(Connection::open(&self.source, self.guard.as_ref())?);
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
            Provider::new(Source::Drs, prefixes(&["drs://"])),
            Provider::new(Source::Synapse, prefixes(&["syn://"])),
            Provider::new(Source::Irods, prefixes(&["irods://"])),
            Provider::new(Source::Oci, prefixes(&["oci://"])),
            Provider::new(Source::Lakefs, prefixes(&["lakefs://"])),
            Provider::new(Source::Lamin, prefixes(&["lamin://"])),
            Provider::new(Source::Quilt, prefixes(&["quilt+s3://"])),
            Provider::new(Source::Cas, prefixes(&["cas://"])),
        ];
        Storage {
            inner: Arc::new(Inner {
                configured: Arc::new(configured),
                built_in: Arc::new(built_in),
                store: settings.store,
                local_store: settings.local_store,
                journal: journal::Journal::default(),
                downloading: Mutex::default(),
                observed: Mutex::default(),
                held: Mutex::default(),
            }),
        }
    }

    /// Shares provider clients with fresh read history, pins, versions and download ownership.
    pub fn session(&self) -> Storage {
        Storage {
            inner: Arc::new(Inner {
                configured: self.inner.configured.clone(),
                built_in: self.inner.built_in.clone(),
                store: self.inner.store.clone(),
                local_store: self.inner.local_store.clone(),
                journal: journal::Journal::default(),
                downloading: Mutex::default(),
                observed: Mutex::default(),
                held: Mutex::default(),
            }),
        }
    }

    /// The same storage with more providers, which serve what the providers before them do
    /// not, like those that a Nextflow config names.
    pub fn with_providers(&self, providers: Vec<settings::ProviderSettings>) -> Storage {
        let mut configured: Vec<Provider> = (self.inner.configured.iter())
            .map(|provider| Provider::new(provider.source.clone(), provider.prefixes.clone()))
            .collect();
        for provider in providers {
            let prefixes = provider.prefixes.clone();
            configured.push(Provider::new(
                Source::Configured(Box::new(provider)),
                prefixes,
            ));
        }
        Storage {
            inner: Arc::new(Inner {
                configured: Arc::new(configured),
                built_in: self.inner.built_in.clone(),
                store: self.inner.store.clone(),
                local_store: self.inner.local_store.clone(),
                journal: journal::Journal::default(),
                downloading: Mutex::default(),
                observed: Mutex::default(),
                held: Mutex::default(),
            }),
        }
    }

    /// The same storage for a confined run: the built-in providers reach only hosts on
    /// public networks or in `allowed_hosts`, and those of web, bucket and FTP addresses stay.
    pub fn public_only(&self, allowed_hosts: &[String]) -> Storage {
        let guard = guard::Guard::new(allowed_hosts);
        let prefixes =
            |prefixes: &[&str]| prefixes.iter().map(|&prefix| prefix.to_owned()).collect();
        let guarded = |source, given: &[&str]| Provider {
            guard: Some(guard.clone()),
            ..Provider::new(source, prefixes(given))
        };
        let built_in = vec![
            guarded(Source::Web, &["http://", "https://"]),
            guarded(Source::PublicBuckets, &["s3://", "gs://"]),
            Provider::new(Source::Files, prefixes(&["file://"])),
            guarded(Source::Ftp, &["ftp://"]),
        ];
        let configured = (self.inner.configured.iter())
            .map(|provider| Provider::new(provider.source.clone(), provider.prefixes.clone()))
            .collect();
        Storage {
            inner: Arc::new(Inner {
                configured: Arc::new(configured),
                built_in: Arc::new(built_in),
                store: self.inner.store.clone(),
                local_store: self.inner.local_store.clone(),
                journal: journal::Journal::default(),
                downloading: Mutex::default(),
                observed: Mutex::default(),
                held: Mutex::default(),
            }),
        }
    }

    /// The providers and the store of the config file.
    pub fn from_config(config: &ConfigFile) -> Result<Storage, ConfigError> {
        Ok(Storage::new(Settings::from_config(config)?))
    }

    /// The account of the `https` provider of the config file that serves an address, for
    /// clients like Git that sign in with a user and a password: its user and password, or
    /// its token as the password. `None` without such a provider or account.
    pub fn account(&self, address: &str) -> Result<Option<(String, String)>, ConfigError> {
        let Some((provider, _)) = longest(&self.inner.configured, address) else {
            return Ok(None);
        };
        let backend::Source::Configured(settings) = &provider.source else {
            return Ok(None);
        };
        let settings::Kind::Https(https) = &settings.kind else {
            return Ok(None);
        };
        let user = https.user.clone();
        Ok(match (&https.password, &https.token) {
            (Some(password), _) => Some((user.unwrap_or_default(), password.reveal()?)),
            // Git servers take a token as the password of any user.
            (None, Some(token)) => Some((user.unwrap_or_else(|| "git".into()), token.reveal()?)),
            (None, None) => None,
        })
    }

    /// The S3 providers of the config file: name, the prefixes they serve, and the address of
    /// their service, `None` for AWS itself. No secrets.
    pub fn s3_providers(&self) -> Vec<(String, Vec<String>, Option<String>)> {
        (self.inner.configured.iter())
            .filter_map(|provider| match &provider.source {
                backend::Source::Configured(settings) => match &settings.kind {
                    settings::Kind::S3(s3) => Some((
                        settings.name.clone(),
                        provider.prefixes.clone(),
                        s3.endpoint.clone(),
                    )),
                    _ => None,
                },
                _ => None,
            })
            .collect()
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

    /// Checks current metadata and selects a download cache for that workflow view.
    pub async fn current(&self, target: &str) -> Result<Option<Metadata>, StorageError> {
        if let Some(pinned) = self.inner.journal.pinned(target) {
            return Ok(Some(Metadata {
                directory: false,
                modified: None,
                size: pinned.size,
                version: pinned.version,
            }));
        }
        let found = self.stat(target).await?;
        let mut observed = self
            .inner
            .observed
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(metadata) = &found {
            observed.insert(
                target.to_owned(),
                Observed {
                    metadata: metadata.clone(),
                    stamp: stamp(metadata),
                },
            );
        } else {
            observed.remove(target);
        }
        Ok(found)
    }

    /// Asks the provider once per session for the file's own version before choosing its cache.
    pub(crate) async fn observe(&self, target: &str) {
        if self.version_known(target) {
            return;
        }
        let turn = self.turn(&format!("version of {target}"));
        let _turn = turn.lock().await;
        if self.version_known(target) {
            return;
        }
        // A file that the provider cannot find or describe gets a download of its own.
        let metadata = self.stat(target).await.ok().flatten();
        let metadata = metadata.unwrap_or(Metadata {
            directory: false,
            modified: None,
            size: 0,
            version: None,
        });
        let stamp = stamp(&metadata);
        let mut observed =
            (self.inner.observed.lock()).unwrap_or_else(|poison| poison.into_inner());
        observed.insert(target.to_owned(), Observed { metadata, stamp });
    }

    /// Whether this session observed metadata for this exact address.
    pub(crate) fn version_known(&self, target: &str) -> bool {
        let observed = (self.inner.observed.lock()).unwrap_or_else(|poison| poison.into_inner());
        observed.contains_key(target)
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
        let (temporary, downloaded) = self.download_private(target, file).await?;
        temporary
            .persist(file)
            .map_err(|error| error::local(file)(error.error))?;
        Ok(downloaded)
    }

    pub(crate) async fn download_private(
        &self,
        target: &str,
        file: &Path,
    ) -> Result<(tempfile::NamedTempFile, Downloaded), StorageError> {
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
        Ok((temporary, downloaded))
    }

    /// Uploads a local file to an address.
    pub async fn upload(&self, file: &Path, target: &str) -> Result<(), StorageError> {
        self.upload_with_receipt(file, target).await.map(|_| ())
    }

    /// Creates a destination atomically, returning false when existing content stays.
    pub async fn upload_absent(&self, file: &Path, target: &str) -> Result<bool, StorageError> {
        let served = self.serve(target)?;
        let result = match &served.connection.backend {
            Backend::Objects(objects) => objects.upload_absent(file, target).await,
            Backend::Folder(folder) => match folder.path(served.prefix, target) {
                Ok(path) => folder.upload_absent(file, &path).await,
                Err(error) => Err(error),
            },
            _ => {
                return Err(StorageError::Unsupported(
                    "the provider cannot create destinations conditionally".into(),
                ));
            }
        };
        result.map_err(|problem| served.failed("write", target, problem))
    }

    /// Uploads a local file and returns the identity reported by that completed write.
    pub async fn upload_with_receipt(
        &self,
        file: &Path,
        target: &str,
    ) -> Result<UploadReceipt, StorageError> {
        let served = self.serve(target)?;
        if !served.connection.writes() {
            let message = format!(
                "cannot write {target}: {} only reads",
                served.provider.source.describe()
            );
            return Err(StorageError::Unsupported(message));
        }
        let done = served.connection.upload(file, served.prefix, target).await;
        let written = done.map_err(|problem| served.failed("write", target, problem))?;
        Ok(UploadReceipt {
            address: target.to_owned(),
            size: written.size,
            version: written.version,
        })
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

/// Reuse requires an identity that the transfer also reports; stat-only modification times
/// cannot bind downloaded bytes, so those copies stay private to one process.
fn stamp(metadata: &Metadata) -> String {
    if metadata.directory || metadata.version.is_none() {
        return format!("fresh {:?}", std::time::SystemTime::now());
    }
    format!("{metadata:?}")
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
