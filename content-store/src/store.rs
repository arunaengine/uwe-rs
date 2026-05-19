//! Puts and gets file contents by digest, cache entries by key, and task outputs by run.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use bytes::Bytes;
use opendal::layers::RetryLayer;
use opendal::services::{Fs, Memory, S3};
use opendal::{ErrorKind, Operator};
use sha2::{Digest as _, Sha256};
use task_model::{Digest, SpecError, Tree};
use tokio::io::AsyncReadExt;

use crate::layout;

/// Files are read and uploaded in parts of this size, so they never sit in memory as a whole.
const PART_SIZE: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Backend(#[from] opendal::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("stored content of {expected} has digest {actual}; the store is corrupt")]
    Corrupt { expected: Digest, actual: Digest },
    #[error("{0}")]
    Location(String),
    #[error("the directory manifest {digest} cannot be read: {message}")]
    Tree { digest: Digest, message: String },
    #[error("invalid directory manifest: {0}")]
    InvalidTree(#[from] SpecError),
}

/// Connection settings for an S3-compatible bucket; credentials come from the environment.
#[derive(Clone, Debug, Default)]
pub struct S3Options {
    pub bucket: String,
    /// A key prefix inside the bucket, so several stores can share one bucket.
    pub prefix: Option<String>,
    pub endpoint: Option<String>,
    pub region: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Store {
    operator: Operator,
    /// The directory of a local store.
    root: Option<std::path::PathBuf>,
}

impl Store {
    pub fn local(root: &std::path::Path) -> Result<Self, StoreError> {
        std::fs::create_dir_all(root)?;
        let root = root.to_str().ok_or_else(|| {
            StoreError::Location(format!("store path `{}` is not UTF-8", root.display()))
        })?;
        let mut store = Self::with(Operator::new(Fs::default().root(root))?)?;
        store.root = Some(root.into());
        Ok(store)
    }

    /// Where an object of a local store is on disk; `None` for other stores.
    pub fn local_path(&self, location: &str) -> Option<std::path::PathBuf> {
        self.root.as_ref().map(|root| root.join(location))
    }

    pub fn s3(options: &S3Options) -> Result<Self, StoreError> {
        // Without OpenDAL's default features nothing installs the HTTP transport for us.
        opendal::install_default();
        let mut builder = S3::default().bucket(&options.bucket);
        if let Some(prefix) = &options.prefix {
            builder = builder.root(prefix);
        }
        if let Some(endpoint) = &options.endpoint {
            builder = builder.endpoint(endpoint);
        }
        if let Some(region) = &options.region {
            builder = builder.region(region);
        }
        Self::with(Operator::new(builder)?)
    }

    /// A store that lives in memory, for tests and dry runs.
    pub fn memory() -> Result<Self, StoreError> {
        Self::with(Operator::new(Memory::default())?)
    }

    fn with(operator: Operator) -> Result<Self, StoreError> {
        Ok(Self {
            operator: operator.layer(RetryLayer::new()),
            root: None,
        })
    }

    pub async fn put_content(&self, bytes: Bytes) -> Result<Digest, StoreError> {
        let digest = Digest::sha256(&bytes);
        let location = layout::content(&digest);
        if !self.operator.exists(&location).await? {
            self.operator.write(&location, bytes).await?;
        }
        Ok(digest)
    }

    /// Hashes a local file, then uploads it in parts unless the store already has its content.
    pub async fn put_file(&self, file: &std::path::Path) -> Result<(Digest, u64), StoreError> {
        let (digest, size) = hash_file(file).await?;
        let location = layout::content(&digest);
        if !self.operator.exists(&location).await? {
            self.upload(&location, file).await?;
        }
        Ok((digest, size))
    }

    /// Uploads a local file in parts to a location.
    async fn upload(&self, location: &str, file: &std::path::Path) -> Result<(), StoreError> {
        let mut reader = tokio::fs::File::open(file).await?;
        let mut writer = self.operator.writer_with(location).chunk(PART_SIZE).await?;
        let mut buffer = vec![0; PART_SIZE];
        loop {
            let read = reader.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            writer
                .write(Bytes::copy_from_slice(&buffer[..read]))
                .await?;
        }
        writer.close().await?;
        Ok(())
    }

    /// Stores a directory manifest by the digest of its canonical form.
    pub async fn put_tree(&self, tree: &Tree) -> Result<Digest, StoreError> {
        let canonical = tree.canonical()?;
        let digest = Digest::sha256(&canonical);
        let location = layout::tree(&digest);
        if !self.operator.exists(&location).await? {
            self.operator.write(&location, canonical).await?;
        }
        Ok(digest)
    }

    /// Reads a directory manifest by digest and checks that it still matches the digest.
    pub async fn get_tree(&self, digest: &Digest) -> Result<Tree, StoreError> {
        let bytes = self.operator.read(&layout::tree(digest)).await?.to_bytes();
        let actual = Digest::sha256(&bytes);
        if &actual != digest {
            let expected = digest.clone();
            return Err(StoreError::Corrupt { expected, actual });
        }
        serde_json::from_slice(&bytes).map_err(|error| StoreError::Tree {
            digest: digest.clone(),
            message: error.to_string(),
        })
    }

    /// Reads content by digest and checks that the bytes still match it.
    pub async fn get_content(&self, digest: &Digest) -> Result<Bytes, StoreError> {
        let bytes = self
            .operator
            .read(&layout::content(digest))
            .await?
            .to_bytes();
        let actual = Digest::sha256(&bytes);
        if &actual != digest {
            let expected = digest.clone();
            return Err(StoreError::Corrupt { expected, actual });
        }
        Ok(bytes)
    }

    pub async fn has_content(&self, digest: &Digest) -> Result<bool, StoreError> {
        Ok(self.operator.exists(&layout::content(digest)).await?)
    }

    /// Writes a cache entry only if none exists, so the first result for a key wins. Returns
    /// whether this call wrote it.
    pub async fn put_entry(
        &self,
        namespace: &str,
        key: &Digest,
        entry: Bytes,
    ) -> Result<bool, StoreError> {
        let location = layout::entry(namespace, key)?;
        let written = self
            .operator
            .write_with(&location, entry)
            .if_not_exists(true)
            .await;
        match written {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == ErrorKind::ConditionNotMatch => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn get_entry(
        &self,
        namespace: &str,
        key: &Digest,
    ) -> Result<Option<Bytes>, StoreError> {
        let location = layout::entry(namespace, key)?;
        match self.operator.read(&location).await {
            Ok(buffer) => Ok(Some(buffer.to_bytes())),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Stores one output file of a task attempt under its original path, reading it from a
    /// local file in parts.
    pub async fn put_output_file(
        &self,
        run: &str,
        task: &str,
        attempt: u32,
        path: &str,
        file: &std::path::Path,
    ) -> Result<String, StoreError> {
        let location = layout::output(run, task, attempt, path)?;
        self.upload(&location, file).await?;
        Ok(location)
    }

    /// Stores one output file of a task attempt under its original path.
    pub async fn put_output(
        &self,
        run: &str,
        task: &str,
        attempt: u32,
        relative: &str,
        bytes: Bytes,
    ) -> Result<String, StoreError> {
        let location = layout::output(run, task, attempt, relative)?;
        self.operator.write(&location, bytes).await?;
        Ok(location)
    }

    pub async fn get_object(&self, location: &str) -> Result<Bytes, StoreError> {
        Ok(self.operator.read(location).await?.to_bytes())
    }
}

async fn hash_file(file: &std::path::Path) -> Result<(Digest, u64), StoreError> {
    let mut reader = tokio::fs::File::open(file).await?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; PART_SIZE];
    let mut size = 0;
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size += read as u64;
    }
    Ok((Digest::from_sha256(&hasher.finalize()), size))
}
