//! Runs task requests: answers them from the cache, or stages their inputs from the store,
//! runs an executor, and stores the outputs and the cache entry.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod outputs;
mod request;
mod runner;
mod staging;

pub use request::{identity, request};
pub use runner::{Runner, RunnerError, StoredFile, TaskResult};
