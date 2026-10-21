//! Runs task requests: answers them from the cache, or stages their inputs, runs them on a
//! backend, and stores the outputs and the cache entry.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

// The futures of attempts nest deeply, deeper than the default limit lets the compiler check.
#![recursion_limit = "256"]

mod check;
pub mod clean;
mod digests;
mod entry;
mod executor;
mod files;
mod inputs;
mod lookup;
mod network;
mod outputs;
mod request;
pub mod resources;
mod runner;
mod settlement;
mod sites;
mod staging;
mod together;

pub use digests::DigestCache;
pub use entry::CacheEntry;
pub use executor::RunnerExecutor;
pub use files::StorageFiles;
pub use lookup::Lookup;
pub use network::NetworkRule;
pub use outputs::resolved;
pub use request::{identity, request};
pub use runner::{
    ContainerResult, Prepared, Runner, RunnerError, Settings, StoredFile, TaskResult,
};
pub use sites::{Site, SiteRule, Sites};
pub use staging::CONTENTS;
