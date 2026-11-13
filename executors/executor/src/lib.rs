//! Runs tasks: the Bash wrapper that every task runs in, its result manifest, and executors;
//! and containers that answer lines, like the Python process of decision 19.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod access;
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
pub mod local;
pub mod manifest;
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

use futures_util::future::BoxFuture;
pub use tokio_util::sync::CancellationToken;

pub use access::Access;
pub use engine::{ContainerEngine, EngineCommand, EngineKind, NoEngine};
pub use image::ImageSource;
pub use job::{CONTROL, ExecutorError, Finished, HOME, Job, Staged, Staging, Usage, WORK};
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
}

/// The object-safe form of [`Backend`], for a backend that a host picks at run time. Every
/// `Backend` is one, and `Arc<dyn DynBackend>` is a `Backend` again.
pub trait DynBackend: Send + Sync + 'static {
    fn pin<'a>(&'a self, image: &'a str) -> BoxFuture<'a, Result<Option<String>, ExecutorError>>;

    fn run<'a>(
        &'a self,
        job: Job<'a>,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Finished, ExecutorError>>;

    fn hold(&self);

    fn release(&self) -> BoxFuture<'_, ()>;
}

impl<B: Backend> DynBackend for B {
    fn pin<'a>(&'a self, image: &'a str) -> BoxFuture<'a, Result<Option<String>, ExecutorError>> {
        Box::pin(Backend::pin(self, image))
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
}

impl Backend for Arc<dyn DynBackend> {
    async fn pin(&self, image: &str) -> Result<Option<String>, ExecutorError> {
        DynBackend::pin(&**self, image).await
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
}
