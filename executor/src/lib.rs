//! Runs tasks: the Bash wrapper that every task runs in, its result manifest, and executors;
//! and containers that answer lines, like the Python process of decision 19.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod access;
mod braces;
pub mod conda;
pub mod conda_cache;
pub mod containers;
pub mod engine;
pub mod http;
mod image;
mod job;
pub mod local;
pub mod manifest;
pub mod poll;
mod pool;
pub mod process;
pub mod registry;
mod stage;
pub mod wrapper;

use std::future::Future;

pub use tokio_util::sync::CancellationToken;

pub use access::Access;
pub use engine::{ContainerEngine, EngineCommand, EngineKind, NoEngine};
pub use job::{CONTROL, ExecutorError, Finished, HOME, Job, Staged, Staging, Usage, WORK};
pub use local::{ContainerRun, LocalContainer};
pub use pool::{Pool, Share};
pub use stage::place;

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
    /// See `docs/design/engine.md` for the full contract.
    fn hold(&self) {}
}
