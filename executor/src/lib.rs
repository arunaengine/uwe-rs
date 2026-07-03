//! Runs tasks: the Bash wrapper that every task runs in, its result manifest, and executors;
//! and containers that answer lines, like the Python process of decision 19.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod braces;
pub mod engine;
mod image;
mod job;
pub mod local;
pub mod manifest;
mod pool;
pub mod process;
pub mod registry;
mod stage;
pub mod wrapper;

use std::future::Future;

use tokio_util::sync::CancellationToken;

pub use engine::{ContainerEngine, EngineKind, NoEngine};
pub use job::{ExecutorError, Finished, Job, Staged, Staging};
pub use local::LocalContainer;
pub use pool::{Pool, Share};

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
}
