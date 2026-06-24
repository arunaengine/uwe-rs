//! Runs tasks: the Bash wrapper that every task runs in, its result manifest, and executors;
//! and containers that answer lines, like the Python process of decision 19.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod braces;
pub mod engine;
mod job;
pub mod local;
pub mod manifest;
mod pool;
pub mod process;
pub mod wrapper;

use task_model::TaskSpec;

pub use engine::{ContainerEngine, EngineKind, NoEngine};
pub use job::{ExecutorError, Finished, Job, Staged, Staging};
pub use pool::{Pool, Share};

/// Runs tasks, one call per task.
pub trait Executor {
    /// Runs a task in a scratch directory called `name`, a name no other task uses, with the
    /// staged files in place.
    fn run(
        &self,
        name: &str,
        spec: &TaskSpec,
        staging: &Staging,
    ) -> Result<Finished, ExecutorError>;
}

impl Executor for local::LocalContainer {
    fn run(
        &self,
        name: &str,
        spec: &TaskSpec,
        staging: &Staging,
    ) -> Result<Finished, ExecutorError> {
        local::LocalContainer::run(self, name, spec, staging)
    }
}
