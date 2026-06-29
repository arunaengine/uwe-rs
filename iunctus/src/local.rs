//! The local container executor: each task runs in Docker or Podman through the task runner,
//! which reuses results from the cache.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use content_store::Store;
use executor::{ContainerEngine, LocalContainer, Pool};
use task_runner::{Runner, RunnerExecutor, Settings};

/// An executor whose results go to the store and whose tasks run in `scratch`, with the
/// container engine of this machine. Tasks without an image get `default_image`.
pub fn executor(
    store: &Path,
    scratch: &Path,
    run: &str,
    default_image: Option<String>,
) -> Result<RunnerExecutor<LocalContainer>, String> {
    let engine = ContainerEngine::find().map_err(|error| error.to_string())?;
    let store = Store::local(store).map_err(|error| error.to_string())?;
    let backend = LocalContainer {
        engine,
        scratch: scratch.to_owned(),
        pool: Pool::of_this_machine(),
    };
    let settings = Settings {
        downloads: scratch.join("downloads"),
        import_inputs: false,
    };
    let runner = Runner::new(store, backend, run, settings);
    Ok(RunnerExecutor::new(runner, default_image))
}
