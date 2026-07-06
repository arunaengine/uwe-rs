//! Storage requests for code that does not run on Tokio, like the drivers, which plan a
//! workflow on a thread of their own. Each request runs on a small runtime of the storage
//! crate, and the calling thread waits for its end, so that this works from any thread.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::{Entry, Metadata, Storage, StorageError};

/// The storage of a process, for callers that wait.
#[derive(Clone)]
pub struct Blocking {
    storage: Storage,
}

impl Blocking {
    pub(crate) fn new(storage: Storage) -> Blocking {
        Blocking { storage }
    }

    /// A local copy of a file; see [`Storage::fetch`].
    pub fn fetch(&self, target: &str, downloads: &Path) -> Result<PathBuf, StorageError> {
        let (storage, target, downloads) = self.owned(target, downloads);
        run(async move { storage.fetch(&target, &downloads).await })
    }

    /// Whether a file exists, known from `downloads` where it can; see [`Storage::metadata`].
    pub fn metadata(
        &self,
        target: &str,
        downloads: &Path,
    ) -> Result<Option<Metadata>, StorageError> {
        let (storage, target, downloads) = self.owned(target, downloads);
        run(async move { storage.metadata(&target, &downloads).await })
    }

    /// The entries of a directory; see [`Storage::list`].
    pub fn list(&self, target: &str) -> Result<Vec<Entry>, StorageError> {
        let (storage, target) = (self.storage.clone(), target.to_owned());
        run(async move { storage.list(&target).await })
    }

    /// Uploads a local file; see [`Storage::upload`].
    pub fn upload(&self, file: &Path, target: &str) -> Result<(), StorageError> {
        let (storage, target, file) = self.owned(target, file);
        run(async move { storage.upload(&file, &target).await })
    }

    fn owned(&self, target: &str, path: &Path) -> (Storage, String, PathBuf) {
        (self.storage.clone(), target.to_owned(), path.to_owned())
    }
}

/// Runs a request on the runtime of the storage crate and waits for its end.
fn run<T: Send + 'static>(
    request: impl Future<Output = Result<T, StorageError>> + Send + 'static,
) -> Result<T, StorageError> {
    let runtime = runtime()?;
    let (sender, receiver) = std::sync::mpsc::channel();
    runtime.spawn(async move {
        // The receiver waits until the request ends.
        let _ = sender.send(request.await);
    });
    receiver.recv().unwrap_or_else(|_| {
        Err(StorageError::Unsupported(
            "the storage request stopped before it ended".to_owned(),
        ))
    })
}

/// The runtime of storage requests that callers wait for, made once per process.
fn runtime() -> Result<&'static tokio::runtime::Runtime, StorageError> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    let runtime = RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("iunctus-storage")
            .enable_all()
            .build()
            .map_err(|error| error.to_string())
    });
    runtime.as_ref().map_err(|message| {
        StorageError::Unsupported(format!(
            "cannot start the runtime of storage requests: {message}"
        ))
    })
}
