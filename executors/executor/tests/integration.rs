//! Runs the integration test modules of this crate in one test binary.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

#[path = "cancellation.rs"]
mod cancellation;
#[path = "conda.rs"]
mod conda;
#[path = "conda_archive.rs"]
mod conda_archive;
#[path = "conda_cache.rs"]
mod conda_cache;
#[path = "image_preparation.rs"]
mod image_preparation;
#[path = "local.rs"]
mod local;
#[path = "process.rs"]
mod process;
#[path = "wrapper.rs"]
mod wrapper;
