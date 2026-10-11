//! Streams task objects into verified local files, exposes remote content addresses and signs
//! requests for machines without credentials.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::time::Duration;

use sha2::{Digest as _, Sha256};
use task_model::Digest;
use tokio::io::AsyncWriteExt;

use crate::store::PART_SIZE;
use crate::{Store, StoreError, layout};

/// A request that a machine without credentials of its own sends to read or write one object,
/// until the time it was signed for has passed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Presigned {
    pub method: String,
    pub url: String,
    /// Headers that the request must send as they are, like `x-ms-blob-type` of Azure.
    pub headers: Vec<(String, String)>,
}

impl Store {
    /// A signed request that reads the object at `location` for `expire`. Only object stores
    /// that sign requests have them, like S3, Google Cloud Storage and Azure Blob Storage.
    pub async fn presign_read(
        &self,
        location: &str,
        expire: Duration,
    ) -> Result<Presigned, StoreError> {
        self.presign(location, expire, false).await
    }

    /// A signed request that writes the object at `location` in one piece for `expire`.
    pub async fn presign_write(
        &self,
        location: &str,
        expire: Duration,
    ) -> Result<Presigned, StoreError> {
        self.presign(location, expire, true).await
    }

    async fn presign(
        &self,
        location: &str,
        expire: Duration,
        write: bool,
    ) -> Result<Presigned, StoreError> {
        let capability = self.operator.info().capability();
        if !(capability.presign_read && capability.presign_write) {
            return Err(StoreError::Location(format!(
                "the store at {} cannot sign requests; use S3, Google Cloud Storage or Azure \
                 Blob Storage",
                self.url("")
            )));
        }
        let request = match write {
            true => self.operator.presign_write(location, expire).await?,
            false => self.operator.presign_read(location, expire).await?,
        };
        let headers = (request.header().iter())
            .map(|(name, value)| {
                let value = value.to_str().map_err(|error| {
                    StoreError::Location(format!("a signed header is not text: {error}"))
                })?;
                Ok((name.as_str().to_owned(), value.to_owned()))
            })
            .collect::<Result<_, StoreError>>()?;
        Ok(Presigned {
            method: request.method().as_str().to_owned(),
            url: request.uri().to_string(),
            headers,
        })
    }

    /// The S3 address of existing content, including the shared layer of a local store.
    pub async fn content_address(&self, digest: &Digest) -> Result<Option<String>, StoreError> {
        let location = layout::content(digest);
        let mut layer = Some(self);
        while let Some(store) = layer {
            if store
                .base
                .as_deref()
                .is_some_and(|base| base.starts_with("s3://"))
                && store.operator.exists(&location).await?
            {
                return Ok(Some(store.url(&location)));
            }
            layer = store.shared.as_deref();
        }
        Ok(None)
    }

    /// Reads a task output atomically, checking its reported size and computing its digest.
    pub async fn read_object(
        &self,
        location: &str,
        version: Option<&str>,
        target: &Path,
        expected_size: Option<u64>,
    ) -> Result<(Digest, u64), StoreError> {
        let mut stat = self.operator.stat_with(location);
        let mut read = self.operator.reader_with(location);
        if let Some(version) = version {
            stat = stat.version(version);
            read = read.version(version);
        }
        let size = stat.await?.content_length();
        if expected_size.is_some_and(|expected| size != expected) {
            return Err(StoreError::Location(
                "TES output size differs from its log".into(),
            ));
        }
        let parent = target.parent().unwrap_or_else(|| Path::new("."));
        tokio::fs::create_dir_all(parent).await?;
        let temporary = tempfile::NamedTempFile::new_in(parent)?;
        let mut file = tokio::fs::File::from_std(temporary.reopen()?);
        let reader = read.await?;
        let mut hash = Sha256::new();
        let mut offset = 0;
        while offset < size {
            let end = (offset + PART_SIZE as u64).min(size);
            let bytes = reader.read(offset..end).await?.to_bytes();
            if bytes.len() as u64 != end - offset {
                return Err(StoreError::Location(
                    "TES output changed size while being read".into(),
                ));
            }
            hash.update(&bytes);
            file.write_all(&bytes).await?;
            offset = end;
        }
        file.flush().await?;
        drop(file);
        temporary.persist(target).map_err(|error| error.error)?;
        Ok((Digest::from_sha256(&hash.finalize()), size))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn verifies_object_size() {
        let store = Store::memory().unwrap();
        let digest = store.put_content(b"output".to_vec().into()).await.unwrap();
        let folder = tempfile::tempdir().unwrap();
        let target = folder.path().join("output");
        let location = layout::content(&digest);
        assert!(
            store
                .read_object(&location, None, &target, Some(7))
                .await
                .is_err()
        );
        assert!(!target.exists());
        assert_eq!(
            store
                .read_object(&location, None, &target, Some(6))
                .await
                .unwrap(),
            (digest, 6)
        );
        assert_eq!(std::fs::read(target).unwrap(), b"output");
    }

    #[tokio::test]
    async fn signs_object_requests() {
        let memory = Store::memory().unwrap();
        let error = memory.presign_read("cas/a", Duration::from_secs(60)).await;
        assert!(error.unwrap_err().to_string().contains("cannot sign"));
        let s3 = opendal::services::S3::default()
            .bucket("bucket")
            .root("staging")
            .endpoint("http://127.0.0.1:9")
            .region("us-east-1")
            .access_key_id("key")
            .secret_access_key("secret");
        let store = Store::remote(opendal::Operator::new(s3).unwrap(), "s3://bucket/staging");
        let store = store.unwrap();
        let read = store.presign_read("cas/a", Duration::from_secs(60)).await;
        let read = read.unwrap();
        assert_eq!(read.method, "GET");
        assert!(read.url.contains("/bucket/staging/cas/a?"), "{}", read.url);
        assert!(read.url.contains("X-Amz-Expires=60"), "{}", read.url);
        let write = store
            .presign_write("files/1", Duration::from_secs(60))
            .await;
        assert_eq!(write.unwrap().method, "PUT");
    }
}
