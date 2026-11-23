//! Runs tasks: the Bash wrapper that every task runs in, its result manifest, and executors;
//! and containers that answer lines, like the Python process of decision 19.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod access;
pub mod attempts;
pub mod batch;
pub mod binary;
mod braces;
pub mod capture;
pub mod conda;
pub mod conda_cache;
pub mod containers;
pub mod credentials;
pub mod daemon;
pub mod engine;
pub mod helper;
pub mod history;
pub mod http;
mod image;
mod job;
pub mod live;
pub mod local;
pub mod manifest;
pub mod multipart;
pub mod orphans;
pub mod poll;
mod pool;
pub mod process;
pub mod registry;
pub mod remote;
mod stage;
#[cfg(test)]
mod test_engine;
pub mod wrapper;

use std::future::Future;
use std::sync::Arc;

pub use futures_util::future::BoxFuture;
pub use tokio_util::sync::CancellationToken;

pub use access::Access;
pub use attempts::{FileJournal, JournalLock, JournalStore, Journals, MemoryJournal};
pub use engine::{ContainerEngine, EngineCommand, EngineKind, NoEngine};
pub use image::ImageSource;
pub use job::{
    CONTROL, DirectSource, ExecutorError, Finished, HOME, Job, OutputReceipt, RECEIPTS, Staged,
    Staging, Usage, WORK,
};
pub use live::{Live, LiveFeed, LiveSink, LogStream};
pub use local::{ContainerRun, LocalContainer};
pub use pool::{Pool, Share};
pub use stage::{place, place_cancellable};

/// Runs the attempts of tasks in containers: on this machine, on a TES server or on a cluster.
pub trait Backend: Send + Sync + 'static {
    /// The image pinned to its digest, like `debian@sha256:...`; `None` for an image without
    /// one, such as one built locally.
    fn pin(
        &self,
        image: &str,
    ) -> impl Future<Output = Result<Option<String>, ExecutorError>> + Send;

    /// The image pinned as the backend of the site `site` pins it, for a backend that sends
    /// tasks to sites; a backend of one site pins it as [`Backend::pin`] does.
    fn pin_at(
        &self,
        image: &str,
        site: Option<&str>,
    ) -> impl Future<Output = Result<Option<String>, ExecutorError>> + Send {
        let _ = site;
        self.pin(image)
    }

    /// Runs one attempt of a task with its inputs in place, and stops it when `cancel` fires.
    fn run(
        &self,
        job: Job<'_>,
        cancel: &CancellationToken,
    ) -> impl Future<Output = Result<Finished, ExecutorError>> + Send;

    /// Starts no attempt that waits for its turn on this machine, as a run that ends after a
    /// failure starts no more tasks; an attempt that waits ends as cancelled.
    fn hold(&self) {}

    /// Waits until the daemons of the run stopped, after their run asked them to stop.
    fn release(&self) -> impl Future<Output = ()> + Send {
        async {}
    }

    /// Whether the workers fetch the inputs of [`Staging::direct`] themselves, from where they
    /// lie. A backend without them gets each input as a local file.
    fn direct_sources(&self) -> bool {
        false
    }

    /// Whether the workers fetch this input of [`Staging::direct`] themselves. One they cannot
    /// fetch, like one whose reference needs headers they cannot send, gets staged as a file.
    fn supports_direct(&self, reference: &storage::DirectRef) -> bool {
        let _ = reference;
        self.direct_sources()
    }

    /// The jobs that the run `run` left on the servers of this backend, with what the servers
    /// say of them. Changes nothing. A backend without journals of its jobs finds none.
    fn orphans(
        &self,
        run: &str,
    ) -> impl Future<Output = Result<Vec<orphans::Orphan>, ExecutorError>> + Send {
        let _ = run;
        async { Ok(Vec::new()) }
    }

    /// Stops the jobs that the run `run` left, as [`Backend::orphans`] finds them, and waits
    /// until their servers confirm it. It keeps a job whose uwe process still runs.
    fn stop_orphans(
        &self,
        run: &str,
    ) -> impl Future<Output = Result<Vec<orphans::Orphan>, ExecutorError>> + Send {
        let _ = run;
        async { Ok(Vec::new()) }
    }
}

/// The object-safe form of [`Backend`], for a backend that a host picks at run time. Every
/// `Backend` is one, and `Arc<dyn DynBackend>` is a `Backend` again.
pub trait DynBackend: Send + Sync + 'static {
    fn pin<'a>(&'a self, image: &'a str) -> BoxFuture<'a, Result<Option<String>, ExecutorError>>;

    fn pin_at<'a>(
        &'a self,
        image: &'a str,
        site: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Option<String>, ExecutorError>>;

    fn run<'a>(
        &'a self,
        job: Job<'a>,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Finished, ExecutorError>>;

    fn hold(&self);

    fn release(&self) -> BoxFuture<'_, ()>;

    fn direct_sources(&self) -> bool;

    fn supports_direct(&self, reference: &storage::DirectRef) -> bool;

    fn orphans<'a>(&'a self, run: &'a str) -> BoxFuture<'a, OrphansResult>;

    fn stop_orphans<'a>(&'a self, run: &'a str) -> BoxFuture<'a, OrphansResult>;
}

/// What [`Backend::orphans`] and [`Backend::stop_orphans`] give.
pub type OrphansResult = Result<Vec<orphans::Orphan>, ExecutorError>;

impl<B: Backend> DynBackend for B {
    fn pin<'a>(&'a self, image: &'a str) -> BoxFuture<'a, Result<Option<String>, ExecutorError>> {
        Box::pin(Backend::pin(self, image))
    }

    fn pin_at<'a>(
        &'a self,
        image: &'a str,
        site: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Option<String>, ExecutorError>> {
        Box::pin(Backend::pin_at(self, image, site))
    }

    fn run<'a>(
        &'a self,
        job: Job<'a>,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Finished, ExecutorError>> {
        Box::pin(Backend::run(self, job, cancel))
    }

    fn hold(&self) {
        Backend::hold(self);
    }

    fn release(&self) -> BoxFuture<'_, ()> {
        Box::pin(Backend::release(self))
    }

    fn direct_sources(&self) -> bool {
        Backend::direct_sources(self)
    }

    fn supports_direct(&self, reference: &storage::DirectRef) -> bool {
        Backend::supports_direct(self, reference)
    }

    fn orphans<'a>(&'a self, run: &'a str) -> BoxFuture<'a, OrphansResult> {
        Box::pin(Backend::orphans(self, run))
    }

    fn stop_orphans<'a>(&'a self, run: &'a str) -> BoxFuture<'a, OrphansResult> {
        Box::pin(Backend::stop_orphans(self, run))
    }
}

impl Backend for Arc<dyn DynBackend> {
    async fn pin(&self, image: &str) -> Result<Option<String>, ExecutorError> {
        DynBackend::pin(&**self, image).await
    }

    async fn pin_at(
        &self,
        image: &str,
        site: Option<&str>,
    ) -> Result<Option<String>, ExecutorError> {
        DynBackend::pin_at(&**self, image, site).await
    }

    async fn run(
        &self,
        job: Job<'_>,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        DynBackend::run(&**self, job, cancel).await
    }

    fn hold(&self) {
        DynBackend::hold(&**self);
    }

    async fn release(&self) {
        DynBackend::release(&**self).await;
    }

    fn direct_sources(&self) -> bool {
        DynBackend::direct_sources(&**self)
    }

    fn supports_direct(&self, reference: &storage::DirectRef) -> bool {
        DynBackend::supports_direct(&**self, reference)
    }

    async fn orphans(&self, run: &str) -> OrphansResult {
        DynBackend::orphans(&**self, run).await
    }

    async fn stop_orphans(&self, run: &str) -> OrphansResult {
        DynBackend::stop_orphans(&**self, run).await
    }
}
