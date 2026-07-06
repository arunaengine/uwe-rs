//! The connection of a provider: its client, made when the provider is first used, with its
//! secrets read then. Built-in providers serve what no provider of the config file serves, so
//! that a machine without a config file works as before: the public web, public buckets over
//! HTTPS and local files. FTP servers need a provider of the config file.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use base64::Engine;
use config_file::{ConfigError, Origin, Secret};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};

use crate::error::Problem;
use crate::folder::Folder;
use crate::public::PublicBuckets;
use crate::settings::{self, Kind, ProviderSettings};
use crate::web::Web;
use crate::{Entry, Metadata, StorageError};

/// Where a provider comes from.
pub(crate) enum Source {
    /// A provider of the config file.
    Configured(Box<ProviderSettings>),
    /// `http` and `https` addresses that no provider serves.
    Web,
    /// `s3` and `gs` addresses that no provider serves.
    PublicBuckets,
    /// `file` addresses.
    Files,
}

impl Source {
    /// The provider in words, for messages.
    pub fn describe(&self) -> String {
        match self {
            Source::Configured(settings) => format!("storage provider `{}`", settings.name),
            Source::Web => "the web, without credentials".to_owned(),
            Source::PublicBuckets => "a public bucket, without credentials".to_owned(),
            Source::Files => "the local file system".to_owned(),
        }
    }
}

/// The client of a provider.
pub(crate) enum Backend {
    Web(Web),
    PublicBuckets(PublicBuckets),
    Folder(Folder),
}

/// A provider's client and the secrets that its messages must not show.
pub(crate) struct Connection {
    pub backend: Backend,
    pub secrets: Vec<String>,
}

impl Connection {
    /// Makes the client of a provider, and reads its secrets.
    pub fn open(source: &Source) -> Result<Connection, StorageError> {
        let failed = |problem: Problem| StorageError::Unsupported(problem.message);
        let mut secrets = Revealed::default();
        let backend = match source {
            Source::Web => Backend::Web(Web::public().map_err(failed)?),
            Source::PublicBuckets => Backend::PublicBuckets(PublicBuckets::new().map_err(failed)?),
            Source::Files => Backend::Folder(Folder {
                root: PathBuf::from("/"),
            }),
            Source::Configured(settings) => match &settings.kind {
                Kind::Https(https) => {
                    let headers = headers(https, &settings.origin, &mut secrets)?;
                    Backend::Web(Web::with_headers(headers).map_err(failed)?)
                }
                Kind::Folder(folder) => Backend::Folder(Folder {
                    root: folder.path.clone(),
                }),
                Kind::S3(_)
                | Kind::GoogleCloudStorage(_)
                | Kind::AzureBlobStorage(_)
                | Kind::Ftp(_) => {
                    return Err(StorageError::Unsupported(format!(
                        "iunctus cannot use the storage provider `{}` yet",
                        settings.name
                    )));
                }
            },
        };
        Ok(Connection {
            backend,
            secrets: secrets.0,
        })
    }

    /// Whether the provider can list the entries of a directory.
    pub fn lists(&self) -> bool {
        !matches!(self.backend, Backend::Web(_))
    }

    /// Whether the provider can write files.
    pub fn writes(&self) -> bool {
        matches!(self.backend, Backend::Folder(_))
    }

    pub async fn stat(&self, prefix: &str, target: &str) -> Result<Option<Metadata>, Problem> {
        match &self.backend {
            Backend::Web(web) => web.stat(target).await,
            Backend::PublicBuckets(buckets) => buckets.stat(target).await,
            Backend::Folder(folder) => folder.stat(&folder.path(prefix, target)?).await,
        }
    }

    pub async fn list(&self, prefix: &str, target: &str) -> Result<Vec<Entry>, Problem> {
        match &self.backend {
            Backend::Web(_) => Err(Problem::fixed("web servers cannot list their files")),
            Backend::PublicBuckets(buckets) => buckets.list(target).await,
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
        offset: u64,
    ) -> Result<bool, Problem> {
        match &self.backend {
            Backend::Web(web) => web.download(target, file, offset).await,
            Backend::PublicBuckets(buckets) => buckets.download(target, file, offset).await,
            Backend::Folder(folder) => folder.download(&folder.path(prefix, target)?, file).await,
        }
    }

    pub async fn upload(&self, file: &Path, prefix: &str, target: &str) -> Result<(), Problem> {
        match &self.backend {
            Backend::Folder(folder) => folder.upload(file, &folder.path(prefix, target)?).await,
            _ => Err(Problem::fixed("the provider only reads")),
        }
    }
}

/// The secrets that a provider has read so far.
#[derive(Default)]
struct Revealed(Vec<String>);

impl Revealed {
    fn reveal(&mut self, secret: &Option<Secret>) -> Result<Option<String>, ConfigError> {
        let Some(secret) = secret else {
            return Ok(None);
        };
        let value = secret.reveal()?;
        self.0.push(value.clone());
        Ok(Some(value))
    }
}

/// The headers that a web provider sends with every request, all marked as sensitive, so that
/// no log shows them.
fn headers(
    https: &settings::Https,
    provider: &Origin,
    secrets: &mut Revealed,
) -> Result<HeaderMap, StorageError> {
    let mut headers = HeaderMap::new();
    let mut add = |name: HeaderName, value: String, origin: &Origin| {
        let mut value = HeaderValue::from_str(&value).map_err(|_| {
            ConfigError::at(origin, "the secret cannot be sent in a header of a request")
        })?;
        value.set_sensitive(true);
        headers.insert(name, value);
        Ok::<(), ConfigError>(())
    };
    if let Some(token) = &https.token
        && let Some(value) = secrets.reveal(&Some(token.clone()))?
    {
        add(AUTHORIZATION, format!("Bearer {value}"), token.origin())?;
    }
    if let Some(user) = &https.user {
        let password = secrets.reveal(&https.password)?.unwrap_or_default();
        let encoded =
            base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
        secrets.0.push(encoded.clone());
        let origin = https.password.as_ref().map_or(provider, Secret::origin);
        add(AUTHORIZATION, format!("Basic {encoded}"), origin)?;
    }
    for (name, secret) in &https.headers {
        let Some(value) = secrets.reveal(&Some(secret.clone()))? else {
            continue;
        };
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
            ConfigError::at(
                secret.origin(),
                format!("`{name}` is not a valid name of a header"),
            )
        })?;
        add(name, value, secret.origin())?;
    }
    Ok(headers)
}
