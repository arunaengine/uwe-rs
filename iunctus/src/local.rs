//! The local container executor behind the engine's executor interface: each task runs in
//! Docker or Podman through the task runner, which reuses results from the cache.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use content_store::Store;
use executor::{ContainerEngine, LocalContainer, Pool};
use task_model::Digest;
use task_runner::{Runner, RunnerError};
use workflow_graph::{ExecuteError, Executor, Outcome, OutputFile, Task};

pub struct Local {
    runner: Runner<LocalContainer>,
    /// The digest of each image, or `None` for an image without one, as found once per run.
    pinned: HashMap<String, Option<String>>,
    /// Prints one line per task when set.
    pub verbose: bool,
}

impl Local {
    /// An executor whose results go to the store and whose tasks run in `scratch`, with the
    /// container engine of this machine.
    pub fn new(store: &Path, scratch: &Path, run: &str) -> Result<Self, String> {
        let engine = ContainerEngine::find().map_err(|error| error.to_string())?;
        let store = Store::local(store).map_err(|error| error.to_string())?;
        let executor = LocalContainer {
            engine,
            scratch: scratch.to_owned(),
            pool: Pool::of_this_machine(),
        };
        let runner = Runner::new(store, executor, run, &scratch.join("downloads"))
            .map_err(|error| error.to_string())?;
        Ok(Local {
            runner,
            pinned: HashMap::new(),
            verbose: true,
        })
    }

    /// The image pinned to its digest when it has one.
    fn pin(&mut self, image: &str) -> Result<String, String> {
        if !self.pinned.contains_key(image) {
            let digest = self
                .runner
                .executor()
                .pin(image)
                .map_err(|error| error.to_string())?;
            self.pinned.insert(image.to_owned(), digest);
        }
        Ok(self.pinned[image]
            .clone()
            .unwrap_or_else(|| image.to_owned()))
    }
}

impl Executor for Local {
    fn execute(&mut self, task: &Task) -> Result<Outcome, ExecuteError> {
        let failed = |error: RunnerError| failure(task, error);
        let image = match &task.image {
            Some(image) => Some(self.pin(image).map_err(|message| ExecuteError::Executor {
                task: task.name.clone(),
                message,
            })?),
            None => None,
        };
        let request = self.runner.request(task, image).map_err(failed)?;
        let result = self.runner.execute(&request).map_err(failed)?;
        if self.verbose {
            let state = if result.reused { "cached" } else { "done" };
            println!("[{state}] {}", task.name);
        }
        let text = |digest: &Option<Digest>| match digest {
            Some(digest) => self
                .runner
                .read_content(digest)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .map_err(failed),
            None => Ok(String::new()),
        };
        let files = result
            .files
            .iter()
            .map(|file| OutputFile {
                path: file.path.clone(),
                location: (self.runner.local_path(&file.location))
                    .unwrap_or_else(|| PathBuf::from(&file.location)),
                directory: file.directory,
            })
            .collect();
        Ok(Outcome {
            id: task.id,
            exit_code: result.exit_code.unwrap_or(-1),
            files,
            stdout: text(&result.stdout)?,
            stderr: text(&result.stderr)?,
        })
    }
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
