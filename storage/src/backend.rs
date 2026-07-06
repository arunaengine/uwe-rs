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
use crate::objects::{Objects, Service};
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
    Objects(Objects),
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
                Kind::S3(s3) => {
                    let service = s3_service(s3, settings, &mut secrets)?;
                    Backend::Objects(Objects::new(service).map_err(failed)?)
                }
                Kind::GoogleCloudStorage(gcs) => {
                    let service = Service::GoogleCloudStorage {
                        endpoint: gcs.endpoint.clone(),
                        credential_file: gcs.credential_file.clone(),
                        token: secrets.reveal(&gcs.token)?,
                        anonymous: gcs.anonymous,
                    };
                    Backend::Objects(Objects::new(service).map_err(failed)?)
                }
                Kind::AzureBlobStorage(azure) => {
                    let endpoint = match (&azure.endpoint, &azure.account) {
                        (Some(endpoint), _) => endpoint.clone(),
                        (None, Some(account)) => format!("https://{account}.blob.core.windows.net"),
                        (None, None) => String::new(),
                    };
                    let service = Service::AzureBlobStorage {
                        endpoint,
                        account: azure.account.clone(),
                        key: secrets.reveal(&azure.key)?,
                        sas_token: secrets.reveal(&azure.sas_token)?,
                    };
                    Backend::Objects(Objects::new(service).map_err(failed)?)
                }
                Kind::Https(https) => {
                    let headers = headers(https, &settings.origin, &mut secrets)?;
                    Backend::Web(Web::with_headers(headers).map_err(failed)?)
                }
                Kind::Folder(folder) => Backend::Folder(Folder {
                    root: folder.path.clone(),
                }),
                Kind::Ftp(_) => {
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
        matches!(self.backend, Backend::Objects(_) | Backend::Folder(_))
    }

    pub async fn stat(&self, prefix: &str, target: &str) -> Result<Option<Metadata>, Problem> {
        match &self.backend {
            Backend::Web(web) => web.stat(target).await,
            Backend::PublicBuckets(buckets) => buckets.stat(target).await,
            Backend::Objects(objects) => objects.stat(target).await,
            Backend::Folder(folder) => folder.stat(&folder.path(prefix, target)?).await,
        }
    }

    pub async fn list(&self, prefix: &str, target: &str) -> Result<Vec<Entry>, Problem> {
        match &self.backend {
            Backend::Web(_) => Err(Problem::fixed("web servers cannot list their files")),
            Backend::PublicBuckets(buckets) => buckets.list(target).await,
            Backend::Objects(objects) => objects.list(target).await,
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
            Backend::Objects(objects) => objects.download(target, file, offset).await,
            Backend::Folder(folder) => folder.download(&folder.path(prefix, target)?, file).await,
        }
    }

    pub async fn upload(&self, file: &Path, prefix: &str, target: &str) -> Result<(), Problem> {
        match &self.backend {
            Backend::Objects(objects) => objects.upload(file, target).await,
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

fn s3_service(
    s3: &settings::S3,
    settings: &ProviderSettings,
    secrets: &mut Revealed,
) -> Result<Service, StorageError> {
    let from_environment = || {
        ["AWS_REGION", "AWS_DEFAULT_REGION"]
            .iter()
            .find_map(|name| std::env::var(name).ok().filter(|region| !region.is_empty()))
    };
    // Services other than AWS mostly ignore the region, but requests must name one.
    let region = (s3.region.clone())
        .or_else(from_environment)
        .or_else(|| s3.endpoint.as_ref().map(|_| "us-east-1".to_owned()))
        .ok_or_else(|| {
            ConfigError::at(
                &settings.origin,
                format!(
                    "the storage provider `{}` needs `region`, the AWS region of its buckets, \
                     like region = \"eu-central-1\", or the environment variable AWS_REGION",
                    settings.name
                ),
            )
        })?;
    let keys = match (&s3.access_key_id, &s3.secret_access_key) {
        (Some(id), Some(secret)) => {
            let id = secrets.reveal(&Some(id.clone()))?.unwrap_or_default();
            let secret = secrets.reveal(&Some(secret.clone()))?.unwrap_or_default();
            Some((id, secret, secrets.reveal(&s3.session_token)?))
        }
        _ => None,
    };
    Ok(Service::S3 {
        endpoint: s3.endpoint.clone(),
        region,
        virtual_host: !s3.path_style.unwrap_or(s3.endpoint.is_some()),
        keys,
        anonymous: s3.anonymous,
    })
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
