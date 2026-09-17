//! The task runner as the executor of a run: each attempt of a task runs on the backend, after
//! the checks that every real executor makes. A task that does nothing finishes at once, and a
//! task without a container image gets the default image or is an error, since every task runs
//! in a container (decision 11). The simulator makes neither check: it pretends every task, so
//! that a simulated run still gets the files its driver expects.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use content_store::Store;
use executor::{Backend, ExecutorError};
use task_model::{Digest, Measured, ReuseScope};
use workflow_graph::{
    Attempt, CancellationToken, ContainerOutcome, ExecuteError, Executor, Files, Network, Outcome,
    OutputFile, ReproducibilityCheck, Running, Task, TaskId, Verdict,
};

use crate::settlement::{Candidate, Completing, Pending};
use crate::{NetworkRule, Runner, RunnerError, SiteRule, StorageFiles, TaskResult};

/// A runner as the executor of a run.
pub struct RunnerExecutor<B> {
    runner: Runner<B>,
    /// The image of tasks that name none, from `-default-image`.
    default_image: Option<String>,
    pending: HashMap<(TaskId, u32), Arc<Pending>>,
    /// Prints a line when a task ends.
    pub verbose: bool,
    /// How soon the tasks of each step should start when tasks wait for the machine: the time
    /// of the longest chain of work that follows from the step in earlier runs, in
    /// milliseconds (see `Placement::priority`). Steps without one start in the order asked.
    pub priorities: Arc<HashMap<String, u64>>,
    /// Whether a stored result may answer a task, and a new result is stored for later runs.
    /// A replay of one task sets it to false, so that the task runs again whatever the cache
    /// holds, and leaves the cache as it was.
    pub reuse: bool,
    /// Which tasks get the network.
    pub network: NetworkRule,
    /// What the tasks of each step used in earlier runs, which right-sizes the CPUs and memory
    /// of the first attempt of a task (see [`crate::resources`]) and tells short tasks.
    pub measured: Arc<HashMap<String, Vec<Measured>>>,
    /// Whether tasks ask for right-sized resources; without it they ask for what their
    /// workflow names.
    pub right_sizing: bool,
    /// Whether short tasks run together: the first attempts of tasks of steps that
    /// [`task_model::per_container`] finds short, with the same image and resources, run one after
    /// another in one container (see `together.rs`).
    pub together: bool,
    /// Which site runs each task, where the backend has several.
    pub sites: SiteRule,
}

impl<B: Backend> RunnerExecutor<B> {
    pub fn new(runner: Runner<B>, default_image: Option<String>) -> Self {
        RunnerExecutor {
            runner,
            default_image,
            pending: HashMap::new(),
            verbose: false,
            priorities: Arc::default(),
            reuse: true,
            network: NetworkRule::default(),
            measured: Arc::default(),
            right_sizing: true,
            together: false,
            sites: SiteRule::default(),
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

    /// Pins images to the digests given; see [`Runner::pin_images`].
    pub fn pin_images(&self, images: &HashMap<String, String>) {
        self.runner.pin_images(images);
    }
}

impl<B: Backend> Executor for RunnerExecutor<B> {
    fn run(&mut self, attempt: Attempt) -> Running {
        self.pending.retain(|(id, _), pending| {
            if *id == attempt.task.id {
                pending.cancel.cancel();
            }
            if pending.cancel.is_cancelled() {
                pending.finish();
            }
            !pending.terminal()
        });
        let task = match checked(&attempt.task, self.default_image.as_deref()) {
            Ok(Some(task)) => with_network(task, &self.network),
            Ok(None) => {
                return Running::ended(Ok(Outcome {
                    id: attempt.task.id,
                    ..Outcome::default()
                }));
            }
            Err(error) => return Running::ended(Err(error)),
        };
        let mut runner = self.runner.clone();
        let pending = Arc::new(Pending {
            task: task.clone(),
            cancel: attempt.cancel.child_token(),
            candidate: Mutex::new(None),
            terminal: Default::default(),
        });
        self.pending
            .insert((task.id, attempt.number), pending.clone());
        runner.verbose_groups = self.verbose;
        let verbose = self.verbose;
        let priority = self.priorities.get(&task.step).copied().unwrap_or_default();
        let reuse = self.reuse;
        let together = self.together;
        let right_sizing = self.right_sizing;
        let site = self.sites.site(&task).map(str::to_owned);
        let measured = self.measured.clone();
        let completing = Completing {
            pending: pending.clone(),
            retained: false,
        };
        Running::new(async move {
            let mut completing = completing;
            let number = attempt.number;
            let measured = measured.get(&task.step).map_or(&[][..], Vec::as_slice);
            let options = AttemptOptions {
                priority,
                reuse,
                measured,
                right_sizing,
                together,
                site,
            };
            let result = outcome(&runner, &task, number, &options, &pending.cancel).await;
            let result = result.map(|(outcome, candidate)| {
                if !pending.cancel.is_cancelled() {
                    completing.retained = candidate.is_some();
                    *pending
                        .candidate
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()) = candidate;
                }
                outcome
            });
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

    fn settle(
        &mut self,
        task: TaskId,
        attempt: u32,
        accepted: bool,
    ) -> Pin<Box<dyn Future<Output = Result<(), ExecuteError>> + Send + 'static>> {
        self.pending.retain(|_, pending| !pending.terminal());
        let settling = self.pending.get(&(task, attempt)).and_then(Pending::take);
        let runner = self.runner.clone();
        Box::pin(async move {
            let Some(mut settling) = settling else {
                return Ok(());
            };
            let candidate = settling.candidate.as_ref().expect("retained candidate");
            let result = runner
                .settle(candidate, accepted, &settling.pending.cancel)
                .await
                .map_err(|error| failure(&settling.pending.task, error));
            if result.is_ok() {
                settling.candidate = None;
                settling.pending.finish();
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

    fn hold(&mut self) {
        self.runner.backend().hold();
    }
}

impl<B> Drop for RunnerExecutor<B> {
    fn drop(&mut self) {
        for pending in self.pending.values() {
            pending.cancel.cancel();
        }
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

/// The task with the network that the rule gives it, which its request passes on to the
/// backend.
fn with_network(task: Arc<Task>, rule: &NetworkRule) -> Arc<Task> {
    let network = match rule.offline(&task) {
        true => Network::Denied,
        false => Network::Allowed,
    };
    let same = task.network == network
        || (task.network == Network::Unstated && network == Network::Allowed);
    match same {
        true => task,
        false => Arc::new(Task {
            network,
            ..Task::clone(&task)
        }),
    }
}

/// How an attempt of a task is placed and whether the cache may answer it.
struct AttemptOptions<'a> {
    priority: u64,
    reuse: bool,
    /// What the earlier tasks of its step used.
    measured: &'a [Measured],
    /// Whether it asks for right-sized resources.
    right_sizing: bool,
    /// Whether it may run with other short tasks, when it is short.
    together: bool,
    /// The site that runs it, where there are several.
    site: Option<String>,
}

/// Runs an attempt of a task through the runner, and turns its result into an outcome.
async fn outcome<B: Backend>(
    runner: &Runner<B>,
    task: &Task,
    attempt: u32,
    options: &AttemptOptions<'_>,
    cancel: &CancellationToken,
) -> Result<(Outcome, Option<Candidate>), ExecuteError> {
    if cancel.is_cancelled() {
        return Err(ExecuteError::Cancelled);
    }
    let failed = |error: RunnerError| failure(task, error);
    let image = match &task.image {
        Some(image) => Some(tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ExecuteError::Cancelled),
            pinned = runner.pin(image) => pinned.map_err(failed)?,
        }),
        None => None,
    };
    // The other containers of a task that runs several are pinned too, so that the key of
    // the task names each image exactly.
    let mut pinned = task.clone();
    for container in pinned.containers.iter_mut().skip(1) {
        if let Some(image) = &container.image {
            container.image = Some(tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(ExecuteError::Cancelled),
                pinned = runner.pin(image) => pinned.map_err(failed)?,
            });
        }
    }
    let mut prepared = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(ExecuteError::Cancelled),
        prepared = runner.request(&pinned, image.clone()) => prepared.map_err(failed)?,
    };
    prepared.request.attempt = attempt;
    if options.right_sizing {
        let placement = &prepared.request.placement;
        prepared.request.placement =
            crate::resources::placement(task, placement, attempt, options.measured);
    }
    prepared.request.placement.priority = options.priority;
    prepared.request.placement.executor = options.site.clone();
    // A retry runs alone.
    if options.together && attempt <= 1 {
        prepared.per_container = task_model::per_container(options.measured);
    }
    if !options.reuse {
        prepared.request.scope = ReuseScope::None;
    }
    let (result, candidate) = runner
        .execute_inner(&prepared, cancel, true)
        .await
        .map_err(failed)?;
    let mut outcome = outcome_of(&runner.store, task, attempt, result, image)
        .await
        .map_err(failed)?;
    outcome.inputs = (prepared.request.spec.inputs.iter())
        .map(|input| (input.path.clone(), input.digest.to_string()))
        .collect();
    Ok((outcome, candidate))
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
            // A path for an object on this machine, else its address.
            location: store.url(&file.location),
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
        containers: containers(store, &result.containers).await?,
        ..Outcome::default()
    })
}

/// How the other containers of a task that runs several ended, with their streams as text.
async fn containers(
    store: &Store,
    results: &[crate::ContainerResult],
) -> Result<Vec<ContainerOutcome>, RunnerError> {
    let text = async |digest: &Option<Digest>| -> Result<String, RunnerError> {
        match digest {
            Some(digest) => {
                let bytes = store.get_content(digest).await?;
                Ok(String::from_utf8_lossy(&bytes).into_owned())
            }
            None => Ok(String::new()),
        }
    };
    let mut outcomes = Vec::new();
    for result in results {
        outcomes.push(ContainerOutcome {
            name: result.name.clone(),
            exit_code: result.exit_code,
            stdout: text(&result.stdout).await?,
            stderr: text(&result.stderr).await?,
        });
    }
    Ok(outcomes)
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

    #[test]
    fn unsupported_backends_do_not_retry_as_machine_failures() {
        let error = RunnerError::Executor(ExecutorError::Unsupported("feature".into()));
        assert!(matches!(
            failure(&task(Some("debian"), &["true"]), error),
            ExecuteError::Unsupported { .. }
        ));
    }

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
