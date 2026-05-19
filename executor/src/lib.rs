//! Runs tasks: the Bash wrapper that every task runs in, its result manifest, and executors.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod local;
pub mod manifest;
pub mod wrapper;

use task_model::TaskSpec;

pub use local::{ExecutorError, Finished, Staged, Staging};

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
