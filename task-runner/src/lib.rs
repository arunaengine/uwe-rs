//! Runs task requests: answers them from the cache, or stages their inputs, runs them on a
//! backend, and stores the outputs and the cache entry.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod check;
pub mod clean;
mod digests;
mod entry;
mod executor;
mod files;
mod inputs;
mod lookup;
mod outputs;
mod request;
mod runner;
mod staging;

pub use digests::DigestCache;
pub use entry::CacheEntry;
pub use executor::RunnerExecutor;
pub use files::StorageFiles;
pub use lookup::Lookup;
pub use request::{identity, request};
pub use runner::{Prepared, Runner, RunnerError, Settings, StoredFile, TaskResult};
