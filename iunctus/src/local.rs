//! The local container executor: each task runs in Docker or Podman through the task runner,
//! which reuses results from the cache.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use content_store::Store;
use executor::{ContainerEngine, LocalContainer, Pool};
use storage::Storage;
use task_runner::{Runner, RunnerExecutor, Settings};

/// An executor whose results go to the store and whose tasks run in `scratch`, with the
/// container engine of this machine. Tasks without an image get `default_image`.
pub fn executor(
    store: Store,
    scratch: &Path,
    run: &str,
    default_image: Option<String>,
    storage: Storage,
) -> Result<RunnerExecutor<LocalContainer>, String> {
    // The client of the engine runs on the runtime of the engine, which runs the attempts.
    let runtime = workflow_graph::runtime().map_err(|error| error.to_string())?;
    let engine = runtime
        .block_on(ContainerEngine::find())
        .map_err(|error| error.to_string())?;
    let backend = LocalContainer {
        engine,
        scratch: scratch.to_owned(),
        pool: Pool::of_this_machine(),
    };
    // Remote inputs share the folder of downloads with the files that workflow code reads.
    let settings = Settings {
        downloads: crate::cache_directory().join("downloads"),
        import_inputs: false,
        storage,
    };
    let runner = Runner::new(store, backend, run, settings);
    Ok(RunnerExecutor::new(runner, default_image))
}
