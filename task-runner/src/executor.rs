//! The task runner as the executor of a run: each attempt of a task runs on the backend, after
//! the checks that every real executor makes. A task that does nothing finishes at once, and a
//! task without a container image gets the default image or is an error, since every task runs
//! in a container (decision 11). The simulator makes neither check: it pretends every task, so
//! that a simulated run still gets the files its driver expects.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::sync::Arc;

use content_store::Store;
use executor::{Backend, ExecutorError};
use task_model::Digest;
use workflow_graph::{
    Attempt, CancellationToken, ExecuteError, Executor, Files, Outcome, OutputFile,
    ReproducibilityCheck, Running, Task, Verdict,
};

use crate::{Runner, RunnerError, StorageFiles, TaskResult};

/// A runner as the executor of a run.
pub struct RunnerExecutor<B> {
    runner: Runner<B>,
    /// The image of tasks that name none, from `-default-image`.
    default_image: Option<String>,
    /// Prints a line when a task ends.
    pub verbose: bool,
    /// How soon the tasks of each step should start when tasks wait for the machine: the time
    /// of the longest chain of work that follows from the step in earlier runs, in
    /// milliseconds (see `Placement::priority`). Steps without one start in the order asked.
    pub priorities: Arc<HashMap<String, u64>>,
}

impl<B: Backend> RunnerExecutor<B> {
    pub fn new(runner: Runner<B>, default_image: Option<String>) -> Self {
        RunnerExecutor {
            runner,
            default_image,
            verbose: false,
            priorities: Arc::default(),
        }
    }

    pub fn runner(&self) -> &Runner<B> {
        &self.runner
    }

    /// Runs a share of the tasks whose results come from the cache again, from 0 (none) to 1
    /// (all), and compares their outputs with the stored ones (the reproducibility check).
    pub fn check_reproducibility(&mut self, share: f64) {
        self.runner.check_reproducibility(share);
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
        let priority = self.priorities.get(&task.step).copied().unwrap_or_default();
        Running::new(async move {
            let result = outcome(&runner, &task, attempt.number, priority, &attempt.cancel).await;
            if verbose && let Ok(outcome) = &result {
                let state = match (outcome.cached, outcome.exit_code) {
                    (true, _) => "cached".to_owned(),
                    (false, 0) => "done".to_owned(),
                    (false, code) => format!("failed with exit code {code}"),
                };
                match &outcome.reproducibility {
                    Some(check) => println!("[checked] {}: {}", task.name, said(check)),
                    None => println!("[{state}] {}", task.name),
                }
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

/// What the reproducibility check of a task found, in words.
fn said(check: &ReproducibilityCheck) -> String {
    let earlier = &check.earlier_run;
    match &check.verdict {
        Verdict::Same => format!("ran again and gave the same bytes as in run {earlier}"),
        Verdict::Differs { paths } => format!(
            "ran again and gave other bytes than in run {earlier}: {}; its tool shares no \
             results any more",
            paths.join(", ")
        ),
        Verdict::Failed { exit_code } => {
            format!("ran again and failed with exit code {exit_code}")
        }
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
    priority: u64,
    cancel: &CancellationToken,
) -> Result<Outcome, ExecuteError> {
    let failed = |error: RunnerError| failure(task, error);
    let image = match &task.image {
        Some(image) => Some(runner.pin(image).await.map_err(failed)?),
        None => None,
    };
    let mut prepared = runner.request(task, image.clone()).await.map_err(failed)?;
    prepared.request.attempt = attempt;
    prepared.request.placement.priority = priority;
    let result = runner.execute(&prepared, cancel).await.map_err(failed)?;
    let mut outcome = outcome_of(&runner.store, task, attempt, result, image)
        .await
        .map_err(failed)?;
    outcome.inputs = (prepared.request.spec.inputs.iter())
        .map(|input| (input.path.clone(), input.digest.to_string()))
        .collect();
    Ok(outcome)
}

/// The outcome of an attempt from its result in the store: each output at its path on this
/// machine for a local store, else at its address, and the captured streams as text.
pub(crate) async fn outcome_of(
    store: &Store,
    task: &Task,
    attempt: u32,
    result: TaskResult,
    image: Option<String>,
) -> Result<Outcome, RunnerError> {
    let text = async |digest: &Option<Digest>| -> Result<String, RunnerError> {
        match digest {
            Some(digest) => {
                let bytes = store.get_content(digest).await?;
                Ok(String::from_utf8_lossy(&bytes).into_owned())
            }
            None => Ok(String::new()),
        }
    };
    let files = result
        .files
        .iter()
        .map(|file| OutputFile {
            path: file.path.clone(),
            location: match store.local_path(&file.location) {
                Some(path) => path.to_string_lossy().into_owned(),
                None => store.url(&file.location),
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
        // The time of the tool itself: waiting for a free CPU and staging come before it.
        started: result.started,
        ended: result.ended,
        usage: result.usage.map(|usage| workflow_graph::Usage {
            cpu_usec: usage.cpu_usec,
            memory_peak: usage.memory_peak,
            read_bytes: usage.read_bytes,
            write_bytes: usage.write_bytes,
        }),
        reproducibility: result.check,
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
