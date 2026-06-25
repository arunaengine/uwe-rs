//! The local container executor behind the engine's executor interface: each task runs in
//! Docker or Podman through the task runner, which reuses results from the cache.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use content_store::Store;
use executor::{ContainerEngine, LocalContainer, Pool};
use task_model::Digest;
use task_runner::{Runner, RunnerError, Settings};
use tokio_util::sync::CancellationToken;
use workflow_graph::{ExecuteError, Executor, Outcome, OutputFile, Task};

pub struct Local {
    runner: Runner<LocalContainer>,
    runtime: tokio::runtime::Runtime,
    /// Prints one line per task when set.
    pub verbose: bool,
}

impl Local {
    /// An executor whose results go to the store and whose tasks run in `scratch`, with the
    /// container engine of this machine.
    pub fn new(store: &Path, scratch: &Path, run: &str) -> Result<Self, String> {
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
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Local {
            runner: Runner::new(store, backend, run, settings),
            runtime,
            verbose: true,
        })
    }
}

impl Executor for Local {
    fn execute(&mut self, task: &Task) -> Result<Outcome, ExecuteError> {
        let (outcome, reused) = self.runtime.block_on(outcome(&self.runner, task))?;
        if self.verbose {
            let state = if reused { "cached" } else { "done" };
            println!("[{state}] {}", task.name);
        }
        Ok(outcome)
    }
}

/// Runs a task through the runner, and turns its result into an outcome; the flag tells whether
/// the store answered it.
async fn outcome(
    runner: &Runner<LocalContainer>,
    task: &Task,
) -> Result<(Outcome, bool), ExecuteError> {
    let failed = |error: RunnerError| failure(task, error);
    let image = match &task.image {
        Some(image) => Some(runner.pin(image).await.map_err(failed)?),
        None => None,
    };
    let prepared = runner.request(task, image).await.map_err(failed)?;
    let cancel = CancellationToken::new();
    let result = runner.execute(&prepared, &cancel).await.map_err(failed)?;
    let text = async |digest: &Option<Digest>| match digest {
        Some(digest) => runner
            .read_content(digest)
            .await
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .map_err(failed),
        None => Ok(String::new()),
    };
    let files = result
        .files
        .iter()
        .map(|file| OutputFile {
            path: file.path.clone(),
            location: (runner.local_path(&file.location))
                .unwrap_or_else(|| PathBuf::from(&file.location)),
            directory: file.directory,
        })
        .collect();
    let outcome = Outcome {
        id: task.id,
        exit_code: result.exit_code.unwrap_or(-1),
        files,
        stdout: text(&result.stdout).await?,
        stderr: text(&result.stderr).await?,
    };
    Ok((outcome, result.reused))
}

/// An error of the runner as an error of the run; remote inputs are not supported yet.
fn failure(task: &Task, error: RunnerError) -> ExecuteError {
    let message = error.to_string();
    if error.unsupported() {
        ExecuteError::Unsupported {
            task: task.name.clone(),
            message,
        }
    } else {
        ExecuteError::Executor {
            task: task.name.clone(),
            message,
        }
    }
}
