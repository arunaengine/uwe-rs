//! The TES executor: each task runs on a GA4GH TES server through the task runner. The store
//! and the scratch directories are on this machine, and the server reads and writes them at
//! the same paths through `file://` URLs, as Funnel on this machine or on a cluster with a
//! shared file system does.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use content_store::Store;
use task_runner::{Runner, RunnerExecutor, Settings};
use tes_executor::{Client, TesBackend};

/// The address of a TES server when `-tes-server` names none: Funnel's default.
pub const DEFAULT_SERVER: &str = "http://localhost:8000";

/// An executor that runs tasks on the TES server at `server`, with the bearer token of
/// `IUNCTUS_TES_TOKEN` when it is set. Tasks without an image get `default_image`.
pub fn executor(
    store: &Path,
    scratch: &Path,
    run: &str,
    default_image: Option<String>,
    server: &str,
) -> Result<RunnerExecutor<TesBackend>, String> {
    let token = std::env::var("IUNCTUS_TES_TOKEN").ok();
    let client = Client::new(server, token).map_err(|error| error.to_string())?;
    let store = Store::local(store).map_err(|error| error.to_string())?;
    let backend = TesBackend {
        client,
        scratch: scratch.join("tes"),
    };
    // The server reads the inputs from the store, so they are copied there.
    let settings = Settings {
        downloads: crate::cache_directory().join("downloads"),
        import_inputs: true,
    };
    let runner = Runner::new(store, backend, run, settings);
    Ok(RunnerExecutor::new(runner, default_image))
}
