//! Object stores through OpenDAL: S3 and the services that speak its API, Google Cloud Storage
//! and Azure Blob Storage. An operator of OpenDAL serves one bucket, so a provider makes one
//! per bucket when it is first used, and all of them share the provider's HTTP client.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use bytes::Bytes;
use futures_util::StreamExt;
use opendal::layers::RetryLayer;
use opendal::services::S3;
use opendal::{ErrorKind, HttpTransporter, OperationContext, Operator};
use opendal_http_transport_reqwest::ReqwestTransport;
use opendal_service_azblob::Azblob;
use opendal_service_gcs::Gcs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::Problem;
use crate::web::{open, shared_client};
use crate::{Downloaded, Entry, Metadata, address};

/// Files are downloaded and uploaded in parts of this size, so they never sit in memory whole.
const PART_SIZE: usize = 8 * 1024 * 1024;

/// How to reach a service, with its secrets revealed.
pub(crate) enum Service {
    S3 {
        endpoint: Option<String>,
        region: String,
        virtual_host: bool,
        /// The access key id, the secret access key and the session token.
        keys: Option<(String, String, Option<String>)>,
        anonymous: bool,
    },
    GoogleCloudStorage {
        endpoint: Option<String>,
        credential_file: Option<PathBuf>,
        token: Option<String>,
        anonymous: bool,
    },
    AzureBlobStorage {
        endpoint: String,
        account: Option<String>,
        key: Option<String>,
        sas_token: Option<String>,
    },
}

pub(crate) struct Objects {
    service: Service,
    transport: HttpTransporter,
    /// The operator of each bucket used so far.
    operators: Mutex<HashMap<String, Operator>>,
}

impl Objects {
    pub fn new(service: Service) -> Result<Objects, Problem> {
        let transport = HttpTransporter::new(ReqwestTransport::new(shared_client()?));
        Ok(Objects {
            service,
            transport,
            operators: Mutex::new(HashMap::new()),
        })
    }

    /// An operator of the bucket whose paths start at `root`.
    fn build(&self, bucket: &str, root: &str) -> Result<Operator, Problem> {
        let operator = match &self.service {
            Service::S3 {
                endpoint,
                region,
                virtual_host,
                keys,
                anonymous,
            } => {
                let mut builder = S3::default().bucket(bucket).root(root).region(region);
                if let Some(endpoint) = endpoint {
                    builder = builder.endpoint(endpoint);
                }
                if *virtual_host {
                    builder = builder.enable_virtual_host_style();
                }
                if let Some((id, secret, token)) = keys {
                    builder = builder
                        .access_key_id(id)
                        .secret_access_key(secret)
                        .disable_config_load()
                        .disable_ec2_metadata();
                    if let Some(token) = token {
                        builder = builder.session_token(token);
                    }
                }
                if *anonymous {
                    builder = builder
                        .skip_signature()
                        .disable_config_load()
                        .disable_ec2_metadata();
                }
                Operator::new(builder)
            }
            Service::GoogleCloudStorage {
                endpoint,
                credential_file,
                token,
                anonymous,
            } => {
                let mut builder = Gcs::default().bucket(bucket).root(root);
                if let Some(endpoint) = endpoint {
                    builder = builder.endpoint(endpoint);
                }
                if let Some(file) = credential_file {
                    builder = builder.credential_path(&file.to_string_lossy());
                }
                if let Some(token) = token {
                    builder = builder.token(token.clone());
                }
                if *anonymous {
                    builder = builder
                        .skip_signature()
                        .disable_config_load()
                        .disable_vm_metadata();
                }
                Operator::new(builder)
            }
            Service::AzureBlobStorage {
                endpoint,
                account,
                key,
                sas_token,
            } => {
                let mut builder = Azblob::default()
                    .container(bucket)
                    .root(root)
                    .endpoint(endpoint);
                if let Some(account) = account {
                    builder = builder.account_name(account);
                }
                if let Some(key) = key {
                    builder = builder.account_key(key);
                }
                if let Some(token) = sas_token {
                    builder = builder.sas_token(token);
                }
                Operator::new(builder)
            }
        };
        let operator = operator.map_err(problem)?;
        let context = OperationContext::new().with_http_transport(self.transport.clone());
        Ok(operator.with_context(context))
    }

    /// The operator of a bucket, made once. It tries again after errors that may go away.
    fn operator(&self, bucket: &str) -> Result<Operator, Problem> {
        let mut operators = self
            .operators
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(operator) = operators.get(bucket) {
            return Ok(operator.clone());
        }
        let operator = self.build(bucket, "/")?.layer(RetryLayer::new());
        operators.insert(bucket.to_owned(), operator.clone());
        Ok(operator)
    }

    /// An operator whose paths start at the address, for a store that keeps its own objects
    /// there and adds its own layers.
    pub fn at(&self, target: &str) -> Result<Operator, Problem> {
        let (bucket, key) = bucket_and_key(target)?;
        self.build(bucket, &format!("/{}", key.trim_matches('/')))
    }

    pub async fn stat(&self, target: &str) -> Result<Option<Metadata>, Problem> {
        let (bucket, key) = bucket_and_key(target)?;
        let operator = self.operator(bucket)?;
        let key = key.trim_end_matches('/');
        if !key.is_empty() {
            match operator.stat(key).await {
                Ok(found) if !found.is_dir() => {
                    let changed = found.last_modified().map(|time| time.to_string());
                    return Ok(Some(Metadata {
                        directory: false,
                        size: found.content_length(),
                        version: found.etag().map(str::to_owned).or(changed),
                    }));
                }
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(problem(error)),
            }
        }
        // A key that is a prefix of other keys is a directory.
        let entries = operator.list(&directory(key)).await;
        match entries {
            Ok(entries) if !entries.is_empty() || key.is_empty() => Ok(Some(Metadata {
                directory: true,
                size: 0,
                version: None,
            })),
            Ok(_) => Ok(None),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(problem(error)),
        }
    }

    pub async fn list(&self, target: &str) -> Result<Vec<Entry>, Problem> {
        let (bucket, key) = bucket_and_key(target)?;
        let folder = directory(key.trim_end_matches('/'));
        let entries = self
            .operator(bucket)?
            .list(&folder)
            .await
            .map_err(problem)?;
        Ok(entries
            .into_iter()
            .filter(|entry| entry.path() != folder)
            .map(|entry| Entry {
                name: entry.name().trim_end_matches('/').to_owned(),
                directory: entry.metadata().is_dir(),
            })
            .collect())
    }

    /// Downloads an object into `file`, from `offset` on, which it always can.
    pub async fn download(
        &self,
        target: &str,
        file: &Path,
        offset: u64,
    ) -> Result<Downloaded, Problem> {
        let (bucket, key) = bucket_and_key(target)?;
        let reader = self.operator(bucket)?.reader(key).await.map_err(problem)?;
        let mut stream = reader.into_bytes_stream(offset..).await.map_err(problem)?;
        let mut output = open(file, offset > 0).await?;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| Problem::passing(error.to_string()))?;
            output
                .write_all(&chunk)
                .await
                .map_err(|error| Problem::local(file, error))?;
        }
        output
            .flush()
            .await
            .map_err(|error| Problem::local(file, error))?;
        Ok(Downloaded { version: None })
    }

    /// Uploads a local file in parts.
    pub async fn upload(&self, file: &Path, target: &str) -> Result<(), Problem> {
        let (bucket, key) = bucket_and_key(target)?;
        let operator = self.operator(bucket)?;
        let mut writer = operator
            .writer_with(key)
            .chunk(PART_SIZE)
            .await
            .map_err(problem)?;
        let mut reader = tokio::fs::File::open(file)
            .await
            .map_err(|error| Problem::local(file, error))?;
        let mut buffer = vec![0; PART_SIZE];
        loop {
            let read = reader
                .read(&mut buffer)
                .await
                .map_err(|error| Problem::local(file, error))?;
            if read == 0 {
                break;
            }
            let part = Bytes::copy_from_slice(&buffer[..read]);
            writer.write(part).await.map_err(problem)?;
        }
        writer.close().await.map_err(problem)?;
        Ok(())
    }
}

fn bucket_and_key(target: &str) -> Result<(&str, &str), Problem> {
    address::bucket_and_key(target).ok_or_else(|| Problem::fixed("it names no bucket"))
}

/// The path that lists a directory: its key with a slash at the end, or `/` for the bucket.
fn directory(key: &str) -> String {
    match key {
        "" => "/".to_owned(),
        key => format!("{key}/"),
    }
}

/// An error of OpenDAL in plain words. Its full text lists the request and the answer, which
/// only helps to debug OpenDAL itself.
fn problem(error: opendal::Error) -> Problem {
    let message = match error.kind() {
        ErrorKind::NotFound => return Problem::missing(),
        ErrorKind::PermissionDenied => {
            "the service refused access; check the credentials of the provider".to_owned()
        }
        ErrorKind::RateLimited => "the service asks for fewer requests".to_owned(),
        ErrorKind::ConfigInvalid => format!(
            "the settings of the provider are wrong: {}",
            error.message()
        ),
        _ => {
            let mut message = error.message().to_owned();
            let mut source = std::error::Error::source(&error);
            while let Some(cause) = source {
                message = format!("{message}: {cause}");
                source = cause.source();
            }
            message
        }
    };
    match error.is_temporary() {
        true => Problem::passing(message),
        false => Problem::fixed(message),
    }
}
