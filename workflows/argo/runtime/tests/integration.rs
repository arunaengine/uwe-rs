//! Runs the integration test modules of this crate in one test binary.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

#[path = "acceptance.rs"]
mod acceptance;
#[path = "adapter.rs"]
mod adapter;
#[path = "artifacts.rs"]
mod artifacts;
#[path = "http.rs"]
mod http;
#[path = "library.rs"]
mod library;
#[path = "loops.rs"]
mod loops;
#[path = "parallelism.rs"]
mod parallelism;
#[path = "remote_files.rs"]
mod remote_files;
#[path = "results.rs"]
mod results;
#[path = "run.rs"]
mod run;
#[path = "schema.rs"]
mod schema;
#[path = "test_workflows.rs"]
mod test_workflows;
#[path = "unsupported.rs"]
mod unsupported;
#[path = "volumes.rs"]
mod volumes;
