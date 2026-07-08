//! The task runner as the executor of a run: each attempt of a task runs on the backend, after
//! the checks that every real executor makes. A task that does nothing finishes at once, and a
//! task without a container image gets the default image or is an error, since every task runs
//! in a container (decision 11). The simulator makes neither check: it pretends every task, so
//! that a simulated run still gets the files its driver expects.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::Arc;

use executor::{Backend, ExecutorError};
use task_model::Digest;
use workflow_graph::{
    Attempt, CancellationToken, ExecuteError, Executor, Files, Outcome, OutputFile, Running, Task,
};

use crate::{Runner, RunnerError, StorageFiles};

/// A runner as the executor of a run.
pub struct RunnerExecutor<B> {
    runner: Runner<B>,
    /// The image of tasks that name none, from `-default-image`.
    default_image: Option<String>,
    /// Prints a line when a task ends.
    pub verbose: bool,
}

impl<B: Backend> RunnerExecutor<B> {
    pub fn new(runner: Runner<B>, default_image: Option<String>) -> Self {
        RunnerExecutor {
            runner,
            default_image,
            verbose: false,
        }
    }

    pub fn runner(&self) -> &Runner<B> {
        &self.runner
    }
}

impl<B: Backend> Executor for RunnerExecutor<B> {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = match checked(&attempt.task, self.default_image.as_deref()) {
            Ok(Some(task)) => task,
            Ok(None) => {
                return Running::ended(Ok(Outcome {
                    id: attempt.task.id,
                    ..Outcome::default()
                }));
            }
            Err(error) => return Running::ended(Err(error)),
        };
        let runner = self.runner.clone();
        let verbose = self.verbose;
        Running::new(async move {
            let result = outcome(&runner, &task, attempt.number, &attempt.cancel).await;
            if verbose && let Ok(outcome) = &result {
                let state = match (outcome.cached, outcome.exit_code) {
                    (true, _) => "cached".to_owned(),
                    (false, 0) => "done".to_owned(),
                    (false, code) => format!("failed with exit code {code}"),
                };
                println!("[{state}] {}", task.name);
            }
            result
        })
    }

    /// The files of the store, wherever it is, which go to paths of this machine and to
    /// addresses of the storage providers.
    fn files(&self) -> Option<Arc<dyn Files>> {
        let settings = &self.runner.settings;
        let files = StorageFiles::new(&settings.storage, settings.downloads.clone());
        Some(Arc::new(files))
    }
}

/// The task to run: the task itself, or with the default image when it names none. `None` for
/// a task that does nothing: it has neither a command nor an image.
fn checked(
    task: &Arc<Task>,
    default_image: Option<&str>,
) -> Result<Option<Arc<Task>>, ExecuteError> {
    if task.is_no_op() {
        return Ok(None);
    }
    match (&task.image, default_image) {
        (Some(_), _) => Ok(Some(task.clone())),
        (None, Some(image)) => Ok(Some(Arc::new(Task {
            image: Some(image.to_owned()),
            ..Task::clone(task)
        }))),
        (None, None) => Err(ExecuteError::Executor {
            task: task.name.clone(),
            message: "the task names no container image; iunctus runs every task in a \
                      container, so name an image in the workflow or give one with \
                      `-default-image <image>`"
                .into(),
        }),
    }
}

/// Runs an attempt of a task through the runner, and turns its result into an outcome.
async fn outcome<B: Backend>(
    runner: &Runner<B>,
    task: &Task,
    attempt: u32,
    cancel: &CancellationToken,
) -> Result<Outcome, ExecuteError> {
    let failed = |error: RunnerError| failure(task, error);
    let image = match &task.image {
        Some(image) => Some(runner.pin(image).await.map_err(failed)?),
        None => None,
    };
    let mut prepared = runner.request(task, image.clone()).await.map_err(failed)?;
    prepared.request.attempt = attempt;
    let result = runner.execute(&prepared, cancel).await.map_err(failed)?;
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
            location: match runner.local_path(&file.location) {
                Some(path) => path.to_string_lossy().into_owned(),
                None => runner.store.url(&file.location),
            },
            directory: file.directory,
            size: Some(file.size),
            digest: Some(file.digest.to_string()),
        })
        .collect();
    Ok(Outcome {
        id: task.id,
        attempt,
        exit_code: result.exit_code.unwrap_or(-1),
        files,
        stdout: text(&result.stdout).await?,
        stderr: text(&result.stderr).await?,
        cached: result.reused,
        image,
        ..Outcome::default()
    })
}

/// An error of the runner as an error of the run: a feature that is not supported yet, a
/// failure of the machine that another attempt may not have, a stopped run, or another error.
fn failure(task: &Task, error: RunnerError) -> ExecuteError {
    if matches!(error, RunnerError::Executor(ExecutorError::Cancelled)) {
        return ExecuteError::Cancelled;
    }
    let (task, message) = (task.name.clone(), error.to_string());
    if error.unsupported() {
        ExecuteError::Unsupported { task, message }
    } else if error.retryable() {
        ExecuteError::Machine { task, message }
    } else {
        ExecuteError::Executor { task, message }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(image: Option<&str>, command: &[&str]) -> Arc<Task> {
        Arc::new(Task {
            name: "t".into(),
            image: image.map(str::to_owned),
            command: command.iter().map(|word| (*word).to_owned()).collect(),
            outputs: vec!["out.txt".into()],
            ..Task::default()
        })
    }

    #[test]
    fn tasks_without_a_command_or_an_image_do_nothing() {
        assert!(checked(&task(None, &[]), None).expect("checked").is_none());
        // An image without a command runs the image's default command.
        let runs = checked(&task(Some("debian"), &[]), None).expect("checked");
        assert!(runs.is_some());
    }

    #[test]
    fn tasks_need_an_image_or_the_default_one() {
        let error = checked(&task(None, &["true"]), None).unwrap_err();
        assert!(
            error.to_string().contains("names no container image"),
            "{error}"
        );
        let image = |task: Option<Arc<Task>>| task.and_then(|task| task.image.clone());
        let given = checked(&task(None, &["true"]), Some("debian")).expect("checked");
        assert_eq!(image(given).as_deref(), Some("debian"));
        let own = checked(&task(Some("alpine"), &["true"]), Some("debian")).expect("checked");
        assert_eq!(image(own).as_deref(), Some("alpine"));
    }
}
