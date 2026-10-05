//! The local container executor: each task runs in Docker or Podman through the task runner,
//! which reuses results from the cache.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use content_store::{Holder, Store};
use executor::{ContainerEngine, LocalContainer, Pool};
use storage::Storage;
use task_runner::{Runner, RunnerExecutor};

/// An executor whose results go to the store and whose tasks run in `scratch`, with the
/// container engine of this machine that `engine` names (`docker`, `podman` or a socket; empty
/// for Docker, else Podman). Tasks without an image get `default_image`.
pub fn executor(
    store: Store,
    scratch: &Path,
    run: &str,
    default_image: Option<String>,
    storage: Storage,
    engine: &str,
    conda: Option<crate::conda::Config>,
) -> Result<RunnerExecutor<crate::conda_backend::Local>, String> {
    // The client of the engine runs on the runtime of the engine, which runs the attempts.
    let runtime = workflow_graph::runtime().map_err(|error| error.to_string())?;
    let named = Some(engine).filter(|name| !name.is_empty());
    let found = runtime.block_on(ContainerEngine::named(named));
    let engine = found.map_err(|error| {
        format!(
            "cannot reach a Docker or Podman engine: {error}; use `-executor simulator` for a run without containers, or `iunctus check-machine` to check the setup"
        )
    })?;
    let backend = LocalContainer::new(engine, scratch.to_owned(), Pool::of_this_machine());
    let backend = crate::conda_backend::Local {
        backend,
        store: store.clone(),
        config: conda,
        holder: Holder::new(run, "Conda image preparation"),
        preparing: std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
    };
    // Remote inputs share the folder of downloads with the files that workflow code reads.
    let runner = Runner::new(store, backend, run, crate::runner_settings(storage, false));
    Ok(RunnerExecutor::new(runner, default_image))
}
