//! Runs asynchronous storage requests for synchronous workflow driver callers.
// Copyright (c) 2026 The uwe Contributors
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

    /// Where a fetched file lives; see [`Storage::download_path`].
    pub fn download_path(&self, target: &str, downloads: &Path) -> Result<PathBuf, StorageError> {
        self.storage.download_path(target, downloads)
    }

    /// The provider-aware bundle name; see [`Storage::download_name`].
    pub fn download_name(&self, target: &str) -> Result<String, StorageError> {
        self.storage.download_name(target)
    }

    /// Locks a folder of downloads shared until the process ends; see [`Storage::hold`].
    pub fn hold(&self, downloads: &Path) -> Result<(), StorageError> {
        let (storage, downloads) = (self.storage.clone(), downloads.to_owned());
        run(async move { storage.hold(&downloads).await })
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

    /// Whether a file exists, and its size; see [`Storage::stat`].
    pub fn stat(&self, target: &str) -> Result<Option<Metadata>, StorageError> {
        let (storage, target) = (self.storage.clone(), target.to_owned());
        run(async move { storage.stat(&target).await })
    }

    pub fn current(&self, target: &str) -> Result<Option<Metadata>, StorageError> {
        let (storage, target) = (self.storage.clone(), target.to_owned());
        run(async move { storage.current(&target).await })
    }

    /// Downloads a file, whole; see [`Storage::download`].
    pub fn download(&self, target: &str, file: &Path) -> Result<(), StorageError> {
        let (storage, target, file) = self.owned(target, file);
        run(async move { storage.download(&target, &file).await })
    }

    /// A copy of a remote file that the process notes as read; see [`Storage::copy_of`].
    pub fn copy_of(&self, target: &str, file: &Path) -> Result<(), StorageError> {
        let (storage, target, file) = self.owned(target, file);
        run(async move { storage.copy_of(&target, &file).await })
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

    /// Creates a destination atomically; see [`Storage::upload_absent`].
    pub fn upload_absent(&self, file: &Path, target: &str) -> Result<bool, StorageError> {
        let (storage, target, file) = self.owned(target, file);
        run(async move { storage.upload_absent(&file, &target).await })
    }

    fn owned(&self, target: &str, path: &Path) -> (Storage, String, PathBuf) {
        (self.storage.clone(), target.to_owned(), path.to_owned())
    }
}

/// The answer of a web server: status code, reason phrase, headers in order and body.
#[derive(Clone, Debug)]
pub struct Answer {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Sends one request with the HTTP client of the process and waits for the whole answer, as
/// scripts that call a web service do. There are no retries.
pub fn exchange(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Option<Vec<u8>>,
) -> Result<Answer, StorageError> {
    let failed = |message: String| StorageError::Failed {
        action: "send a request to",
        address: url.to_owned(),
        provider: "web".to_owned(),
        message,
        retryable: false,
    };
    let client = crate::http_client().map_err(StorageError::Unsupported)?;
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|error| failed(error.to_string()))?;
    let mut request = client.request(method, url);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    if let Some(body) = body {
        request = request.body(body);
    }
    let (address, request) = (
        url.to_owned(),
        request.build().map_err(|e| failed(e.to_string()))?,
    );
    run(async move {
        let failed = |message: String| StorageError::Failed {
            action: "send a request to",
            address: address.clone(),
            provider: "web".to_owned(),
            message,
            retryable: false,
        };
        let response = client
            .execute(request)
            .await
            .map_err(|e| failed(e.to_string()))?;
        let status = response.status();
        let headers = (response.headers().iter())
            .map(|(name, value)| {
                let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
                (name.to_string(), value)
            })
            .collect();
        let body = response.bytes().await.map_err(|e| failed(e.to_string()))?;
        Ok(Answer {
            status: status.as_u16(),
            reason: status.canonical_reason().unwrap_or_default().to_owned(),
            headers,
            body: body.to_vec(),
        })
    })
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
            .thread_name("uwe-storage")
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
