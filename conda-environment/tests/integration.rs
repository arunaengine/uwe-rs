//! Runs the integration test modules of this crate in one test binary.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

#[path = "acquisition.rs"]
mod acquisition;
#[path = "fetch.rs"]
mod fetch;
#[path = "image.rs"]
mod image;
#[path = "image_cache.rs"]
mod image_cache;
#[path = "lock.rs"]
mod lock;
#[path = "pip.rs"]
mod pip;
#[path = "pip_build.rs"]
mod pip_build;
#[path = "pip_lock.rs"]
mod pip_lock;
#[path = "test_workflows.rs"]
mod test_workflows;
