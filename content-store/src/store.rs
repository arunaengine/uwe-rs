//! Puts and gets file contents by digest, cache entries by key, and task outputs by run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::PermissionsExt;

use bytes::Bytes;
use opendal::layers::RetryLayer;
use opendal::services::{Fs, Memory, S3};
use opendal::{ErrorKind, Operator};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use task_model::{Digest, SpecError, Tree};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

impl StoreError {
    pub fn missing(&self) -> bool {
        match self {
            Self::Backend(error) => error.kind() == ErrorKind::NotFound,
            Self::Io(error) | Self::File { error, .. } => {
                error.kind() == std::io::ErrorKind::NotFound
            }
            _ => false,
        }
    }
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredDirectory {
    pub location: String,
    pub digest: Digest,
}

/// Cache entry bytes paired with the versions that may be invalidated after validation.
#[derive(Debug)]
pub struct EntrySnapshot {
    bytes: Bytes,
    location: String,
    version: Option<EntryVersion>,
    shared: Option<Box<EntrySnapshot>>,
}

impl EntrySnapshot {
    pub fn bytes(&self) -> &Bytes {
        &self.bytes
    }
}

#[derive(Debug)]
enum EntryVersion {
    File(std::fs::File),
    Memory,
    Tag(String),
    Unsupported,
}

// Memory has no conditional deletion; all entry mutations cooperate within this process.
static MEMORY_ENTRIES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) struct EntryGuard {
    _file: Option<crate::Lease>,
    _memory: Option<tokio::sync::MutexGuard<'static, ()>>,
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
    pub(crate) cache_reads: bool,
    pub(crate) repair_reads: bool,
    staging: Option<std::path::PathBuf>,
    pub(crate) verified: std::sync::Arc<std::sync::Mutex<crate::layers::Verified>>,
}

impl Store {
    /// Reads without publishing cached copies, creating entry locks, or repairing corruption.
    pub fn read_only(&self) -> Self {
        let mut store = self.without_read_cache();
        store.repair_reads = false;
        store.shared = self
            .shared
            .as_ref()
            .map(|shared| std::sync::Arc::new(shared.read_only()));
        store
    }

    /// Selects storage for private import snapshots instead of the system temporary folder.
    pub fn with_staging(mut self, path: std::path::PathBuf) -> Self {
        self.shared = self
            .shared
            .as_ref()
            .map(|shared| std::sync::Arc::new(shared.as_ref().clone().with_staging(path.clone())));
        self.staging = Some(path);
        self
    }

    /// The local layer alone, for eviction protected by local reader leases.
    pub fn front(&self) -> Option<Self> {
        self.root.as_ref()?;
        let mut store = self.clone();
        store.shared = None;
        Some(store)
    }

    /// Clones this store without publishing read-through data into its front layer.
    pub fn without_read_cache(&self) -> Self {
        let mut store = self.clone();
        store.cache_reads = false;
        store
    }

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
            cache_reads: true,
            repair_reads: true,
            staging: None,
            verified: Default::default(),
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
            None if !self.holds(location, bytes.len() as u64).await? => {
                self.operator.write(location, bytes.clone()).await?;
            }
            None => {}
        }
        if let Some(shared) = &self.shared {
            Box::pin(shared.put_absent(location, bytes)).await?;
        }
        Ok(())
    }

    /// Whether an object store has an object of `size` bytes at `location`. One of another
    /// size is damaged, and the write that asks replaces it.
    async fn holds(&self, location: &str, size: u64) -> Result<bool, StoreError> {
        match self.operator.stat(location).await {
            Ok(metadata) => Ok(metadata.content_length() == size),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// Writes an object, replacing what is there; tasks may write the same one at once.
    pub(crate) async fn replace(&self, location: &str, bytes: &[u8]) -> Result<(), StoreError> {
        match self.local_path(location) {
            Some(target) => local::replace(&target, bytes).await.map_err(at(&target))?,
            None => {
                self.operator.write(location, bytes.to_vec()).await?;
            }
        }
        Ok(())
    }

    /// Copies a local file, then hashes and stores those same bytes.
    pub async fn put_file(&self, file: &std::path::Path) -> Result<(Digest, u64), StoreError> {
        let snapshot = self.import_snapshot()?;
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
        let snapshot = self.import_snapshot()?;
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

    fn import_snapshot(&self) -> Result<tempfile::NamedTempFile, StoreError> {
        let folder = self
            .staging
            .clone()
            .or_else(|| self.root.as_ref().map(|root| root.join("imports")))
            .unwrap_or_else(std::env::temp_dir);
        std::fs::create_dir_all(&folder).map_err(at(&folder))?;
        tempfile::NamedTempFile::new_in(&folder).map_err(at(&folder))
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
            None => {
                let size = tokio::fs::metadata(file).await.map_err(at(file))?.len();
                if !self.holds(&location, size).await? {
                    self.upload(&location, file).await?;
                }
            }
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
                if !self.holds(&content, size).await? {
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

    /// Publishes a manifest association after every directory member has been stored.
    pub async fn put_directory(&self, location: &str, digest: &Digest) -> Result<(), StoreError> {
        self.get_tree(digest).await?;
        let directory = StoredDirectory {
            location: location.to_owned(),
            digest: digest.clone(),
        };
        let bytes = serde_json::to_vec(&directory).map_err(std::io::Error::other)?;
        let key = layout::directory(location);
        if let Some(shared) = &self.shared {
            shared.operator.write(&key, bytes.clone()).await?;
        }
        self.operator.write(&key, bytes).await?;
        Ok(())
    }

    /// The current manifest of a registered directory; legacy locations have no descriptor.
    pub async fn directory(&self, location: &str) -> Result<Option<Digest>, StoreError> {
        let source = self.shared.as_deref().unwrap_or(self);
        let key = layout::directory(location);
        let bytes = match source.operator.read(&key).await {
            Ok(bytes) => bytes.to_bytes(),
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let directory = directory_record(&key, &bytes)?;
        if directory.location != location {
            return Err(StoreError::Location(format!(
                "directory descriptor {key} names another location"
            )));
        }
        if self.cache_reads && self.shared.is_some() {
            self.replace(&key, &bytes).await?;
        }
        Ok(Some(directory.digest))
    }

    /// The directory association in this layer, without fetching or writing a shared copy.
    pub async fn cached_directory(&self, location: &str) -> Result<Option<Digest>, StoreError> {
        let key = layout::directory(location);
        let bytes = match self.operator.read(&key).await {
            Ok(bytes) => bytes.to_bytes(),
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let directory = directory_record(&key, &bytes)?;
        if directory.location != location {
            return Err(StoreError::Location(format!(
                "directory descriptor {key} names another location"
            )));
        }
        Ok(Some(directory.digest))
    }

    /// Directory descriptors with their local inventory metadata, for cleanup retention.
    pub async fn directory_records(
        &self,
    ) -> Result<Vec<(crate::Object, StoredDirectory)>, StoreError> {
        let mut directories = Vec::new();
        for object in self.objects("directories/v1/").await? {
            if object.folder {
                continue;
            }
            let bytes = self.operator.read(&object.location).await?.to_bytes();
            let directory = directory_record(&object.location, &bytes)?;
            directories.push((object, directory));
        }
        Ok(directories)
    }

    /// Reads a directory manifest by digest and checks that it still matches the digest.
    pub async fn get_tree(&self, digest: &Digest) -> Result<Tree, StoreError> {
        let location = layout::tree(digest);
        let bytes = self.read_found(&location).await?;
        let actual = Digest::sha256(&bytes);
        if &actual != digest {
            // The error of the read matters more than one of the repair.
            let _ = self.repair(&location, digest).await;
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
        let location = layout::content(digest);
        let bytes = self.read_found(&location).await?;
        let actual = Digest::sha256(&bytes);
        if &actual != digest {
            let _ = self.repair(&location, digest).await;
            let expected = digest.clone();
            return Err(StoreError::Corrupt { expected, actual });
        }
        Ok(bytes)
    }

    /// Removes corrupt copies only when deletion can preserve a concurrent replacement.
    /// Read-only views and remote providers without conditional deletion retain them.
    pub async fn repair(&self, location: &str, digest: &Digest) -> Result<(), StoreError> {
        if !self.repair_reads {
            return Ok(());
        }
        match self.local_path(location) {
            Some(path) => repair_file(&path, digest).await?,
            None => match repair_object(&self.operator, location, digest).await {
                Ok(()) => {}
                Err(error) if error.missing() => {}
                Err(error) => return Err(error),
            },
        }
        if let Some(shared) = &self.shared {
            Box::pin(shared.repair(location, digest)).await?;
        }
        Ok(())
    }

    /// Streams verified content to a file, replacing its target only after a complete read.
    pub async fn materialize_content(
        &self,
        digest: &Digest,
        target: &std::path::Path,
    ) -> Result<(), StoreError> {
        self.materialize(&layout::content(digest), digest, target, true, None)
            .await
    }

    /// Streams verified content only when its stored size fits the caller's byte allowance.
    pub async fn materialize_content_bounded(
        &self,
        digest: &Digest,
        target: &std::path::Path,
        max_bytes: u64,
    ) -> Result<(), StoreError> {
        self.materialize(
            &layout::content(digest),
            digest,
            target,
            true,
            Some(max_bytes),
        )
        .await
    }

    /// Streams verified output bytes without publishing a partial directory in the front layer.
    pub async fn materialize_object(
        &self,
        location: &str,
        digest: &Digest,
        target: &std::path::Path,
    ) -> Result<(), StoreError> {
        self.materialize(location, digest, target, false, None)
            .await
    }

    async fn materialize(
        &self,
        location: &str,
        digest: &Digest,
        target: &std::path::Path,
        cache: bool,
        max_bytes: Option<u64>,
    ) -> Result<(), StoreError> {
        let (source, metadata) = match self.operator.stat(location).await {
            Ok(metadata) => (self, metadata),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                let Some(shared) = &self.shared else {
                    return Err(error.into());
                };
                (shared.as_ref(), shared.operator.stat(location).await?)
            }
            Err(error) => return Err(error.into()),
        };
        if max_bytes.is_some_and(|limit| metadata.content_length() > limit) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "stored content exceeds its byte limit",
            )
            .into());
        }
        let parent = target
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(at(parent))?;
        // The file gets the ordinary mode of new files, so that tasks of other users read it.
        let temporary = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o666))
            .tempfile_in(parent)
            .map_err(at(target))?;
        let mut file = tokio::fs::File::from_std(temporary.reopen().map_err(at(target))?);
        let reader = source.operator.reader(location).await?;
        let mut hasher = Sha256::new();
        let size = metadata.content_length();
        let mut offset = 0;
        while offset < size {
            let end = (offset + PART_SIZE as u64).min(size);
            let bytes = reader.read(offset..end).await?.to_bytes();
            if bytes.len() as u64 != end - offset {
                return Err(opendal::Error::new(
                    ErrorKind::Unexpected,
                    "stored content changed size while being read",
                )
                .into());
            }
            hasher.update(&bytes);
            file.write_all(&bytes).await.map_err(at(target))?;
            offset = end;
        }
        let actual = Digest::from_sha256(&hasher.finalize());
        if &actual != digest {
            if addressed(location) {
                let _ = source.repair(location, digest).await;
            }
            return Err(StoreError::Corrupt {
                expected: digest.clone(),
                actual,
            });
        }
        file.flush().await.map_err(at(target))?;
        drop(file);
        if cache
            && self.cache_reads
            && !std::ptr::eq(source, self)
            && let Some(cached) = self.local_path(location)
        {
            local::copy_absent(temporary.path(), &cached)
                .await
                .map_err(at(&cached))?;
        }
        temporary
            .persist(target)
            .map_err(|error| at(target)(error.error))?;
        Ok(())
    }

    /// Reads an object in parts and checks it against its digest without keeping its bytes,
    /// and gives its size. A store in layers reads its folder, else the shared store.
    pub async fn verify_object(&self, location: &str, digest: &Digest) -> Result<u64, StoreError> {
        let source = match self.operator.stat(location).await {
            Ok(_) => self,
            Err(error) if error.kind() == ErrorKind::NotFound => match &self.shared {
                Some(shared) => shared.as_ref(),
                None => return Err(error.into()),
            },
            Err(error) => return Err(error.into()),
        };
        let (actual, size) = hash_object(&source.operator, location).await?;
        if &actual != digest {
            if addressed(location) {
                let _ = source.repair(location, digest).await;
            }
            let expected = digest.clone();
            return Err(StoreError::Corrupt { expected, actual });
        }
        Ok(size)
    }

    /// The size of the stored content of a digest, here or in the shared store behind.
    pub async fn content_size(&self, digest: &Digest) -> Result<Option<u64>, StoreError> {
        match self.operator.stat(&layout::content(digest)).await {
            Ok(metadata) => Ok(Some(metadata.content_length())),
            Err(error) if error.kind() == ErrorKind::NotFound => match &self.shared {
                Some(shared) => Box::pin(shared.content_size(digest)).await,
                None => Ok(None),
            },
            Err(error) => Err(error.into()),
        }
    }

    /// The last `limit` bytes of the stored content of a digest, without reading the rest.
    /// They are not checked against the digest, which covers all bytes.
    pub async fn content_tail(&self, digest: &Digest, limit: u64) -> Result<Bytes, StoreError> {
        let location = layout::content(digest);
        match self.operator.stat(&location).await {
            Ok(metadata) => {
                let start = metadata.content_length().saturating_sub(limit);
                let read = self.operator.read_with(&location).range(start..).await?;
                Ok(read.to_bytes())
            }
            Err(error) if error.kind() == ErrorKind::NotFound => match &self.shared {
                Some(shared) => Box::pin(shared.content_tail(digest, limit)).await,
                None => Err(error.into()),
            },
            Err(error) => Err(error.into()),
        }
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
        let _guard = self.entry_guard(&location).await?;
        // The shared winner replaces a front copy hydrated before stale invalidation.
        if self.shared.is_some() {
            self.operator.delete(&location).await?;
        }
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
        let _guard = self.entry_guard(&location).await?;
        self.read(&location).await
    }

    /// Reads an entry together with its mutation version. An existing front copy needs no
    /// shared access; its matching shared version is observed only if invalidation is needed.
    pub async fn observe_entry(
        &self,
        namespace: &str,
        key: &Digest,
    ) -> Result<Option<EntrySnapshot>, StoreError> {
        let location = layout::entry(namespace, key)?;
        let here = self.observe_entry_here(&location).await?;
        if here.is_some() {
            return Ok(here);
        }
        let Some(shared) = &self.shared else {
            return Ok(here);
        };
        let Some(behind) = shared.observe_entry_here(&location).await? else {
            return Ok(None);
        };
        if self.cache_reads
            && let Some(target) = self.local_path(&location)
        {
            local::write_absent(&target, &behind.bytes)
                .await
                .map_err(at(&target))?;
            if let Some(mut front) = self.observe_entry_here(&location).await? {
                front.shared = Some(Box::new(behind));
                return Ok(Some(front));
            }
        }
        Ok(Some(EntrySnapshot {
            bytes: behind.bytes.clone(),
            location,
            version: None,
            shared: Some(Box::new(behind)),
        }))
    }

    async fn observe_entry_here(
        &self,
        location: &str,
    ) -> Result<Option<EntrySnapshot>, StoreError> {
        let (bytes, version) = if let Some(path) = self.local_path(location) {
            let mut file = match tokio::fs::File::open(&path).await {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(at(&path)(error)),
            };
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).await.map_err(at(&path))?;
            (bytes.into(), EntryVersion::File(file.into_std().await))
        } else if self.operator.info().scheme() == "memory" {
            match self.operator.read(location).await {
                Ok(bytes) => (bytes.to_bytes(), EntryVersion::Memory),
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error.into()),
            }
        } else {
            let capability = self.operator.info().capability();
            let conditional = capability.read_with_if_match && capability.delete_with_if_match;
            let mut attempts = 0;
            loop {
                attempts += 1;
                let tag = match self.operator.stat(location).await {
                    Ok(metadata) => metadata.etag().map(str::to_owned),
                    Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
                    Err(error) => return Err(error.into()),
                };
                let tag = tag.filter(|_| conditional);
                let read = match &tag {
                    Some(tag) => self.operator.read_with(location).if_match(tag).await,
                    None => self.operator.read(location).await,
                };
                match read {
                    Ok(bytes) => {
                        break (
                            bytes.to_bytes(),
                            tag.map_or(EntryVersion::Unsupported, EntryVersion::Tag),
                        );
                    }
                    Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
                    Err(error) if error.kind() == ErrorKind::ConditionNotMatch && attempts < 4 => {
                        continue;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        };
        Ok(Some(EntrySnapshot {
            bytes,
            location: location.to_owned(),
            version: Some(version),
            shared: None,
        }))
    }

    /// Removes only the observed stale versions, including a matching shared entry.
    pub async fn invalidate_entry(
        &self,
        namespace: &str,
        key: &Digest,
        observed: &EntrySnapshot,
    ) -> Result<bool, StoreError> {
        if !self.repair_reads {
            return Ok(false);
        }
        let location = layout::entry(namespace, key)?;
        if observed.location != location {
            return Err(StoreError::Location(
                "cache entry observation belongs to another key".into(),
            ));
        }
        let mut removed = false;
        if let Some(shared) = &self.shared {
            let refreshed = if observed.shared.is_none() {
                shared.observe_entry_here(&location).await?
            } else {
                None
            };
            if let Some(behind) = observed.shared.as_deref().or(refreshed.as_ref())
                && behind.bytes == observed.bytes
            {
                removed = Box::pin(shared.invalidate_entry(namespace, key, behind)).await?;
            }
        }
        let _guard = self.entry_guard(&location).await?;
        let deleted = match &observed.version {
            Some(EntryVersion::File(file)) => {
                use std::os::unix::fs::MetadataExt;
                let Some(path) = self.local_path(&location) else { return Ok(removed) };
                let current = match tokio::fs::metadata(&path).await {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(removed),
                    Err(error) => return Err(at(&path)(error)),
                };
                let old = file.metadata().map_err(at(&path))?;
                if (current.dev(), current.ino()) != (old.dev(), old.ino())
                    || tokio::fs::read(&path).await.map_err(at(&path))? != observed.bytes
                {
                    return Ok(removed);
                }
                self.operator.delete(&location).await
            }
            Some(EntryVersion::Memory) if self.operator.info().scheme() == "memory" => {
                match self.operator.read(&location).await {
                    Ok(bytes) if bytes.to_bytes() == observed.bytes => self.operator.delete(&location).await,
                    Ok(_) => return Ok(removed),
                    Err(error) => Err(error),
                }
            }
            Some(EntryVersion::Tag(tag)) => self.operator.delete_with(&location).if_match(tag).await,
            Some(EntryVersion::Unsupported) => return Err(opendal::Error::new(
                ErrorKind::Unsupported,
                "stale cache entry recovery requires conditional reads and deletion with entity tags",
            ).into()),
            _ => return Ok(removed),
        };
        match deleted {
            Ok(()) => Ok(true),
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::NotFound | ErrorKind::ConditionNotMatch
                ) =>
            {
                Ok(removed)
            }
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) async fn entry_guard(&self, location: &str) -> Result<EntryGuard, StoreError> {
        let mut guard = EntryGuard {
            _file: None,
            _memory: None,
        };
        if !self.repair_reads {
            return Ok(guard);
        }
        if self.root.is_some() {
            let mut local = self.clone();
            local.shared = None;
            let name = format!("entries/{}", Digest::sha256(location.as_bytes()).hex());
            loop {
                if let Some(lease) = local.try_lease(&name, &crate::Holder::default()).await? {
                    guard._file = Some(lease);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        } else if self.operator.info().scheme() == "memory" {
            guard._memory = Some(MEMORY_ENTRIES.lock().await);
        }
        Ok(guard)
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

fn directory_record(key: &str, bytes: &[u8]) -> Result<StoredDirectory, StoreError> {
    let directory: StoredDirectory = serde_json::from_slice(bytes).map_err(|error| {
        StoreError::Location(format!("invalid directory descriptor {key}: {error}"))
    })?;
    if layout::directory(&directory.location) != key {
        return Err(StoreError::Location(format!(
            "directory descriptor {key} names another location"
        )));
    }
    Ok(directory)
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

/// Whether a location names its object by the digest of its bytes: a content or a manifest.
fn addressed(location: &str) -> bool {
    location.starts_with("cas/") || location.starts_with("trees/")
}

async fn repair_object(
    operator: &Operator,
    location: &str,
    digest: &Digest,
) -> Result<(), StoreError> {
    let capability = operator.info().capability();
    if !(capability.read_with_if_match && capability.delete_with_if_match) {
        return Ok(());
    }
    let metadata = operator.stat(location).await?;
    let Some(tag) = metadata
        .etag()
        .filter(|tag| !tag.is_empty() && !tag.starts_with("W/"))
    else {
        return Ok(());
    };
    let result = async {
        let reader = operator.reader_with(location).if_match(tag).await?;
        let mut hasher = Sha256::new();
        let mut offset = 0;
        while offset < metadata.content_length() {
            let end = (offset + PART_SIZE as u64).min(metadata.content_length());
            let bytes = reader.read(offset..end).await?.to_bytes();
            if bytes.len() as u64 != end - offset {
                return Err(opendal::Error::new(
                    ErrorKind::Unexpected,
                    "incomplete repair read",
                ));
            }
            hasher.update(&bytes);
            offset = end;
        }
        if Digest::from_sha256(&hasher.finalize()) != *digest {
            operator.delete_with(location).if_match(tag).await?;
        }
        Ok(())
    }
    .await;
    match result {
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::ConditionNotMatch | ErrorKind::NotFound
            ) =>
        {
            Ok(())
        }
        result => Ok(result?),
    }
}

/// The digest and size of an object, read in parts.
async fn hash_object(operator: &Operator, location: &str) -> Result<(Digest, u64), StoreError> {
    let size = operator.stat(location).await?.content_length();
    let reader = operator.reader(location).await?;
    let mut hasher = Sha256::new();
    let mut offset = 0;
    while offset < size {
        let end = (offset + PART_SIZE as u64).min(size);
        let bytes = reader.read(offset..end).await?.to_bytes();
        if bytes.is_empty() {
            break;
        }
        hasher.update(&bytes);
        offset += bytes.len() as u64;
    }
    Ok((Digest::from_sha256(&hasher.finalize()), offset))
}

/// How long one read of a file may take before uwe opens the file again. On CephFS a read of
/// a file that another node wrote can wait forever for its read rights (CVE-2026-80527, Linux
/// before 7.1.10), until other traffic on the file wakes it.
const READ_NUDGE: std::time::Duration = std::time::Duration::from_secs(10);

/// How often a read is woken before it counts as lost.
const READ_NUDGES: u32 = 30;

/// The digest and size of a local file, read in parts.
pub async fn hash_file(file: &std::path::Path) -> Result<(Digest, u64), StoreError> {
    hash_patiently(file, READ_NUDGE, READ_NUDGES).await
}

/// [`hash_file`] that wakes a read which takes longer than `nudge`, up to `nudges` times.
async fn hash_patiently(
    file: &std::path::Path,
    nudge: std::time::Duration,
    nudges: u32,
) -> Result<(Digest, u64), StoreError> {
    let mut reader = tokio::fs::File::open(file).await?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; PART_SIZE];
    let mut size = 0;
    loop {
        let pending = reader.read(&mut buffer);
        tokio::pin!(pending);
        let mut woken = 0;
        let read = loop {
            match tokio::time::timeout(nudge, &mut pending).await {
                Ok(read) => break read?,
                Err(_) if woken < nudges => {
                    woken += 1;
                    wake(file, nudge).await;
                }
                Err(_) => {
                    let message = format!(
                        "the file system did not answer a read for {} seconds",
                        nudge.as_secs() * u64::from(nudges + 1)
                    );
                    return Err(StoreError::File {
                        path: file.to_owned(),
                        error: std::io::Error::new(std::io::ErrorKind::TimedOut, message),
                    });
                }
            }
        };
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size += read as u64;
    }
    Ok((Digest::from_sha256(&hasher.finalize()), size))
}

/// Opens a file again and reads its first byte at offset 0, which takes nothing from a
/// stream: the request for read rights wakes a read of the file that waits for them.
async fn wake(file: &std::path::Path, nudge: std::time::Duration) {
    let path = file.to_owned();
    let opened = tokio::task::spawn_blocking(move || {
        use std::os::unix::fs::FileExt;
        let mut byte = [0];
        std::fs::File::open(&path)?.read_at(&mut byte, 0)
    });
    let _ = tokio::time::timeout(nudge, opened).await;
}

/// Removes a local copy that does not match its digest, if it is still the file that was read:
/// a good write may put a new file in its place meanwhile, which must stay.
async fn repair_file(path: &std::path::Path, digest: &Digest) -> Result<(), StoreError> {
    let read = match tokio::fs::symlink_metadata(path).await {
        Ok(found) => found,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(at(path)(error)),
    };
    match hash_file(path).await {
        Ok((actual, _)) if &actual == digest => Ok(()),
        Ok(_) => remove_read(path, &read).await,
        Err(error) if error.missing() => Ok(()),
        Err(error) => Err(error),
    }
}

/// Moves a file aside and removes it when it is the file of `read`; another file goes back to
/// its place, unless a newer one is there already.
async fn remove_read(path: &std::path::Path, read: &std::fs::Metadata) -> Result<(), StoreError> {
    use std::os::unix::fs::MetadataExt;
    let stamp = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH))
        .map_or(0, |since| since.as_nanos());
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let aside = path.with_file_name(format!(".{name}.{}-{stamp}.repair", std::process::id()));
    match tokio::fs::rename(path, &aside).await {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        result => result.map_err(at(path))?,
    }
    let moved = tokio::fs::symlink_metadata(&aside)
        .await
        .map_err(at(&aside))?;
    if (moved.dev(), moved.ino()) != (read.dev(), read.ino()) {
        let _ = tokio::fs::hard_link(&aside, path).await;
    }
    tokio::fs::remove_file(&aside).await.map_err(at(&aside))
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

    /// A read that waits is woken, and one that never ends fails instead of waiting forever.
    #[tokio::test(flavor = "multi_thread")]
    async fn wakes_stuck_reads() {
        use std::io::Write;
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("fifo");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &path,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();
        let pause = std::time::Duration::from_millis(50);
        let target = path.clone();
        let writer = std::thread::spawn(move || {
            let mut fifo = std::fs::OpenOptions::new()
                .write(true)
                .open(target)
                .unwrap();
            std::thread::sleep(pause * 6);
            fifo.write_all(b"late").unwrap();
        });
        let (digest, size) = hash_patiently(&path, pause, 20).await.unwrap();
        writer.join().unwrap();
        assert_eq!((digest, size), (Digest::sha256(b"late"), 4));
        let target = path.clone();
        let (release, held) = std::sync::mpsc::channel::<()>();
        let writer = std::thread::spawn(move || {
            let _fifo = std::fs::OpenOptions::new()
                .write(true)
                .open(target)
                .unwrap();
            let _ = held.recv();
        });
        let error = hash_patiently(&path, pause, 2).await.unwrap_err();
        assert!(
            error.to_string().contains("did not answer a read"),
            "{error}"
        );
        release.send(()).unwrap();
        writer.join().unwrap();
    }

    #[tokio::test]
    async fn repair_keeps_rewrites() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("object");
        std::fs::write(&path, b"damaged").unwrap();
        let read = std::fs::symlink_metadata(&path).unwrap();
        // A good write puts a new file in place after repair read the damaged one.
        let good = folder.path().join("good");
        std::fs::write(&good, b"content").unwrap();
        std::fs::rename(&good, &path).unwrap();
        remove_read(&path, &read).await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"content");
        let digest = Digest::sha256(b"content");
        std::fs::write(&path, b"damaged").unwrap();
        repair_file(&path, &digest).await.unwrap();
        assert!(!path.exists(), "a damaged copy goes");
        let names = std::fs::read_dir(folder.path()).unwrap().count();
        assert_eq!(names, 0, "nothing stays aside");
    }

    #[test]
    fn explains_service_errors() {
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
