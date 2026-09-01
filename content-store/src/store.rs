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

use crate::{layout, local};

/// Files are read and uploaded in parts of this size, so they never sit in memory as a whole.
pub(crate) const PART_SIZE: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{}", plain(.0))]
    Backend(#[from] opendal::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// A file of this machine could not be read or written.
    #[error("{}: {error}", path.display())]
    File {
        path: std::path::PathBuf,
        error: std::io::Error,
    },
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

/// An output of a task attempt in the store: where it is, and its digest and size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredOutput {
    pub location: String,
    pub digest: Digest,
    pub size: u64,
}

#[derive(Clone, Debug)]
pub struct Store {
    pub(crate) operator: Operator,
    /// The directory of a local store.
    pub(crate) root: Option<std::path::PathBuf>,
    /// The URL of the store's root for others, like `s3://bucket/prefix`.
    pub(crate) base: Option<String>,
    /// For a store in layers, the shared store behind this one in a folder: new objects go
    /// there too, and it serves what the folder lacks (see `layers.rs`).
    pub(crate) shared: Option<std::sync::Arc<Store>>,
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
        let mut store = Self::with(Operator::new(builder)?)?;
        let prefix = options.prefix.as_deref().unwrap_or("").trim_matches('/');
        store.base = Some(match prefix.is_empty() {
            true => format!("s3://{}", options.bucket),
            false => format!("s3://{}/{prefix}", options.bucket),
        });
        Ok(store)
    }

    /// A store in an object store, through an operator whose paths start at the store's root,
    /// like one of a storage provider. `base` is the address of that root for others, like
    /// `s3://bucket/prefix`.
    pub fn remote(operator: Operator, base: &str) -> Result<Self, StoreError> {
        let mut store = Self::with(operator)?;
        store.base = Some(base.trim_end_matches('/').to_owned());
        Ok(store)
    }

    /// Where an object is for others: its path for a local store, its URL for S3, else the
    /// location in the store. A store in layers gives the path in its folder when the object is
    /// there, else the place in the shared store.
    pub fn url(&self, location: &str) -> String {
        if let (Some(shared), Some(root)) = (&self.shared, &self.root)
            && !root.join(location).exists()
        {
            return shared.url(location);
        }
        match (&self.root, &self.base) {
            (Some(root), _) => root.join(location).to_string_lossy().into_owned(),
            (None, Some(base)) => format!("{base}/{location}"),
            (None, None) => location.to_owned(),
        }
    }

    /// A store that lives in memory, for tests and dry runs.
    pub fn memory() -> Result<Self, StoreError> {
        Self::with(Operator::new(Memory::default())?)
    }

    fn with(operator: Operator) -> Result<Self, StoreError> {
        Ok(Self {
            operator: operator.layer(RetryLayer::new()),
            root: None,
            base: None,
            shared: None,
        })
    }

    pub async fn put_content(&self, bytes: Bytes) -> Result<Digest, StoreError> {
        let digest = Digest::sha256(&bytes);
        self.put_absent(&layout::content(&digest), bytes).await?;
        Ok(digest)
    }

    /// Writes an object unless it exists. A local store writes a temporary file and renames
    /// it, so that tasks that store the same content at the same moment do not collide.
    async fn put_absent(&self, location: &str, bytes: Bytes) -> Result<(), StoreError> {
        match self.local_path(location) {
            Some(target) => local::write_absent(&target, &bytes)
                .await
                .map_err(at(&target))?,
            None if !self.operator.exists(location).await? => {
                self.operator.write(location, bytes.clone()).await?;
            }
            None => {}
        }
        if let Some(shared) = &self.shared {
            Box::pin(shared.put_absent(location, bytes)).await?;
        }
        Ok(())
    }

    /// Copies a local file, then hashes and stores those same bytes.
    pub async fn put_file(&self, file: &std::path::Path) -> Result<(Digest, u64), StoreError> {
        let snapshot = tempfile::NamedTempFile::new()?;
        tokio::fs::copy(file, snapshot.path())
            .await
            .map_err(at(file))?;
        let (digest, size) = hash_file(snapshot.path()).await?;
        self.publish_file_as(snapshot.path(), &digest).await?;
        Ok((digest, size))
    }

    /// Stores a copied local file after verifying its expected digest.
    pub async fn put_file_as(
        &self,
        file: &std::path::Path,
        digest: &Digest,
    ) -> Result<(), StoreError> {
        let snapshot = tempfile::NamedTempFile::new()?;
        tokio::fs::copy(file, snapshot.path())
            .await
            .map_err(at(file))?;
        let (actual, _) = hash_file(snapshot.path()).await?;
        if &actual != digest {
            return Err(StoreError::Corrupt {
                expected: digest.clone(),
                actual,
            });
        }
        self.publish_file_as(snapshot.path(), digest).await
    }

    async fn publish_file_as(
        &self,
        file: &std::path::Path,
        digest: &Digest,
    ) -> Result<(), StoreError> {
        let location = layout::content(digest);
        match self.local_path(&location) {
            Some(target) => local::copy_absent(file, &target)
                .await
                .map_err(at(&target))?,
            None if !self.operator.exists(&location).await? => {
                self.upload(&location, file).await?;
            }
            None => {}
        }
        if let Some(shared) = &self.shared {
            Box::pin(shared.publish_file_as(file, digest)).await?;
        }
        Ok(())
    }

    /// Stores an output file of a task attempt under its original path and by its content. A
    /// local store moves the file there and hard-links its content, so nothing is copied; the
    /// file is gone from where it was. Other stores get it uploaded.
    pub async fn adopt_output(
        &self,
        run: &str,
        task: &str,
        attempt: u32,
        path: &str,
        file: &std::path::Path,
    ) -> Result<StoredOutput, StoreError> {
        let location = layout::output(run, task, attempt, path)?;
        let (digest, size) = hash_file(file).await?;
        let content = layout::content(&digest);
        match (self.local_path(&location), self.local_path(&content)) {
            (Some(target), Some(stored)) => {
                local::move_file(file, &target).await.map_err(at(file))?;
                local::link_absent(&target, &stored)
                    .await
                    .map_err(at(&stored))?;
                if let Some(shared) = &self.shared {
                    shared.upload(&location, &target).await?;
                    Box::pin(shared.put_file_as(&target, &digest)).await?;
                }
            }
            _ => {
                self.upload(&location, file).await?;
                if !self.operator.exists(&content).await? {
                    self.upload(&content, file).await?;
                }
            }
        }
        Ok(StoredOutput {
            location,
            digest,
            size,
        })
    }

    /// Uploads a local file in parts to a location.
    async fn upload(&self, location: &str, file: &std::path::Path) -> Result<(), StoreError> {
        let mut reader = tokio::fs::File::open(file).await.map_err(at(file))?;
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
        self.put_absent(&layout::tree(&digest), canonical.into())
            .await?;
        Ok(digest)
    }

    /// Reads a directory manifest by digest and checks that it still matches the digest.
    pub async fn get_tree(&self, digest: &Digest) -> Result<Tree, StoreError> {
        let bytes = self.read_found(&layout::tree(digest)).await?;
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
        let bytes = self.read_found(&layout::content(digest)).await?;
        let actual = Digest::sha256(&bytes);
        if &actual != digest {
            let expected = digest.clone();
            return Err(StoreError::Corrupt { expected, actual });
        }
        Ok(bytes)
    }

    pub async fn has_content(&self, digest: &Digest) -> Result<bool, StoreError> {
        self.exists(&layout::content(digest)).await
    }

    /// Writes a cache entry only if none exists, so the first result for a key wins. Returns
    /// whether this call wrote it. In a store in layers, the shared store decides which result
    /// is first, and the folder keeps a copy of it.
    pub async fn put_entry(
        &self,
        namespace: &str,
        key: &Digest,
        entry: Bytes,
    ) -> Result<bool, StoreError> {
        if let Some(shared) = &self.shared
            && !Box::pin(shared.put_entry(namespace, key, entry.clone())).await?
        {
            return Ok(false);
        }
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
        self.read(&location).await
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
        if let Some(shared) = &self.shared {
            shared.upload(&location, file).await?;
        }
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
        self.operator.write(&location, bytes.clone()).await?;
        if let Some(shared) = &self.shared {
            shared.operator.write(&location, bytes).await?;
        }
        Ok(location)
    }

    pub async fn get_object(&self, location: &str) -> Result<Bytes, StoreError> {
        self.read_found(location).await
    }

    /// The bytes of an object that must be there, from the folder of a store in layers or the
    /// shared store behind it.
    async fn read_found(&self, location: &str) -> Result<Bytes, StoreError> {
        match self.read(location).await? {
            Some(bytes) => Ok(bytes),
            None => Err(opendal::Error::new(ErrorKind::NotFound, location.to_owned()).into()),
        }
    }
}

/// An error of the store's service in plain words: what went wrong and its causes, without the
/// request and the answer that the full text of OpenDAL lists, which only help to debug it.
fn plain(error: &opendal::Error) -> String {
    let mut message = match error.kind() {
        ErrorKind::NotFound => format!("the store has no {}", error.message()),
        ErrorKind::PermissionDenied => "the store refused access; check the credentials of its \
                                        storage provider"
            .to_owned(),
        ErrorKind::RateLimited => "the store asks for fewer requests".to_owned(),
        _ => format!("the store failed: {}", error.message()),
    };
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        message = format!("{message}: {cause}");
        source = cause.source();
    }
    message
}

/// The digest and size of a local file, read in parts.
pub async fn hash_file(file: &std::path::Path) -> Result<(Digest, u64), StoreError> {
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

/// An error of a file operation on this machine, with the path of the file.
fn at(path: &std::path::Path) -> impl FnOnce(std::io::Error) -> StoreError + '_ {
    move |error| StoreError::File {
        path: path.to_owned(),
        error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_of_the_service_are_in_plain_words() {
        let error = opendal::Error::new(ErrorKind::NotFound, "cas/sha256/ab/cd")
            .with_operation("read")
            .with_context("service", "s3");
        assert_eq!(
            StoreError::from(error).to_string(),
            "the store has no cas/sha256/ab/cd"
        );
        let refused = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        let error = opendal::Error::new(ErrorKind::Unexpected, "send http request")
            .with_context("url", "http://127.0.0.1:1/lab")
            .set_source(refused);
        assert_eq!(
            StoreError::from(error).to_string(),
            "the store failed: send http request: refused"
        );
    }
}
