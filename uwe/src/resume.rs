//! Reuses recorded task results after verifying inputs, outputs and execution identity.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use run_database::{Recorder, Reusable, fingerprint, input_states};
use task_model::Digest;
use task_runner::NetworkRule;
use workflow_graph::{
    Attempt, Event, Executor, Files, Network, Observer, Outcome, Reuse, Running, Task, TaskId,
    Writable,
};

/// How strongly a lineage result must be verified before it is used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Real results must still have the exact recorded bytes and immutable execution identity.
    Real,
    /// Simulated results have no bytes or resolved image identity to verify.
    Simulated,
}

/// The tasks whose results came from an earlier run, and that run, until the recorder hears of
/// them.
#[derive(Clone, Default)]
pub struct Taken(Arc<Mutex<HashMap<TaskId, String>>>);

impl Taken {
    fn note(&self, task: TaskId, run: &str) {
        let mut taken = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        taken.insert(task, run.to_owned());
    }

    fn take(&self, task: TaskId) -> Option<String> {
        let mut taken = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        taken.remove(&task)
    }
}

/// An executor that first looks for the task among the finished tasks of the earlier runs.
pub struct Resumed {
    executor: Box<dyn Executor>,
    /// The finished tasks of the lineage, by fingerprint.
    reusable: HashMap<String, Reusable>,
    taken: Taken,
    mode: Mode,
    network: NetworkRule,
    /// Prints one line per task taken from an earlier run when set.
    verbose: bool,
}

impl Resumed {
    pub fn new(
        executor: Box<dyn Executor>,
        reusable: HashMap<String, Reusable>,
        taken: Taken,
        mode: Mode,
        network: NetworkRule,
        verbose: bool,
    ) -> Self {
        Resumed {
            executor,
            reusable,
            taken,
            mode,
            network,
            verbose,
        }
    }
}

impl Executor for Resumed {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = &attempt.task;
        if task.reuse != Reuse::Allowed {
            return self.executor.run(attempt);
        }
        let key = fingerprint(task, &input_states(task));
        let files = self.executor.files();
        let found = self.reusable.get(&key).filter(|found| match self.mode {
            Mode::Real => verified(task, &found.outcome, files.as_deref(), &self.network),
            Mode::Simulated => found
                .outcome
                .files
                .iter()
                .all(|file| std::path::Path::new(&file.location).exists()),
        });
        let Some(found) = found else {
            tracing::debug!(task = %task.name, "resume cache miss");
            return self.executor.run(attempt);
        };
        tracing::debug!(task = %task.name, run = %found.run, "resume cache hit");
        if self.verbose {
            println!("[resumed] {} (from run {})", task.name, found.run);
        }
        self.taken.note(task.id, &found.run);
        Running::ended(Ok(Outcome {
            id: task.id,
            cached: true,
            ..found.outcome.clone()
        }))
    }

    /// The files of the executor that runs the other tasks, so that a resumed run delivers
    /// its results too.
    fn files(&self) -> Option<Arc<dyn Files>> {
        self.executor.files()
    }

    fn hold(&mut self) {
        self.executor.hold();
    }

    fn settle(
        &mut self,
        task: TaskId,
        attempt: u32,
        accepted: bool,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<(), workflow_graph::ExecuteError>>
                + Send
                + 'static,
        >,
    > {
        self.executor.settle(task, attempt, accepted)
    }

    /// The daemons of the tasks that ran stop before the run ends.
    fn release(&mut self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        self.executor.release()
    }
}

/// Whether a real result still has every identity that this wrapper can prove. Unsupported
/// identities safely miss so that the underlying runner can resolve and execute the task.
fn verified(
    task: &Task,
    outcome: &Outcome,
    files: Option<&dyn Files>,
    network: &NetworkRule,
) -> bool {
    // A result of a task that may use the network holds when the task gets it now; whether
    // it had the network then does not matter, as for Nextflow and Snakemake.
    let network_matches = match task.network {
        Network::Unstated | Network::Allowed => !network.offline(task),
        Network::Denied => network.offline(task),
    };
    if outcome.accepted != Some(true) || !network_matches || !task.containers.is_empty() {
        return false;
    }
    let Some(image) = task.image.as_deref().filter(|image| {
        image
            .rsplit_once('@')
            .is_some_and(|(_, digest)| digest.parse::<Digest>().is_ok())
    }) else {
        return false;
    };
    if outcome.image.as_deref() != Some(image) || !inputs_match(task, outcome) {
        return false;
    }
    if outcome.files.is_empty() {
        return true;
    }
    let Some(files) = files else {
        return false;
    };
    outcome.files.iter().all(|file| {
        if file.directory {
            return false;
        }
        let (Some(size), Some(expected)) = (file.size, file.digest.as_deref()) else {
            return false;
        };
        let Ok(expected) = expected.parse::<Digest>() else {
            return false;
        };
        let Ok(local) = files.local(&file.location) else {
            return false;
        };
        let Ok(metadata) = std::fs::metadata(&local) else {
            return false;
        };
        metadata.is_file()
            && hash(&local)
                .is_some_and(|(actual, actual_size)| actual == expected && actual_size == size)
    })
}

fn inputs_match(task: &Task, outcome: &Outcome) -> bool {
    if task.inputs.len() != outcome.inputs.len() {
        return false;
    }
    task.inputs.iter().all(|input| {
        let Some(expected) = outcome.inputs.get(&input.path) else {
            return false;
        };
        let Ok(expected) = expected.parse::<Digest>() else {
            return false;
        };
        let actual = match &input.content {
            Some(content) if !input.directory => Digest::sha256(content.as_bytes()),
            // The runner keys a shared folder by its path.
            None if input.writable == Writable::Shared => {
                let Ok(place) = std::fs::canonicalize(&input.source) else {
                    return false;
                };
                Digest::sha256(place.to_string_lossy().as_bytes())
            }
            Some(_) => return false,
            None if input.directory
                || input.source.contains("://")
                || input.writable == Writable::InPlace =>
            {
                return false;
            }
            None => {
                let source = std::path::Path::new(&input.source);
                let Ok(metadata) = std::fs::metadata(source) else {
                    return false;
                };
                if !metadata.is_file() {
                    return false;
                }
                let Some((digest, _)) = hash(source) else {
                    return false;
                };
                digest
            }
        };
        actual == expected
    })
}

fn hash(path: &std::path::Path) -> Option<(Digest, u64)> {
    let runtime = workflow_graph::runtime().ok()?;
    runtime.block_on(content_store::hash_file(path)).ok()
}

/// Passes the events of the engine to the recorder, and marks the results that came from an
/// earlier run.
pub struct Noting<'a> {
    recorder: &'a mut Recorder,
    taken: Taken,
}

impl<'a> Noting<'a> {
    pub fn new(recorder: &'a mut Recorder, taken: Taken) -> Self {
        Noting { recorder, taken }
    }
}

impl Observer for Noting<'_> {
    fn event(&mut self, event: Event<'_>) {
        let reused = match &event {
            Event::Finished {
                outcome, attempt, ..
            } => self
                .taken
                .take(outcome.id)
                .map(|run| (outcome.id, *attempt, run)),
            _ => None,
        };
        self.recorder.event(event);
        if let Some((task, attempt, run)) = reused {
            self.recorder.reused(task, attempt, &run);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use std::future::Future;
    use workflow_graph::{CancellationToken, Delivery, Input, OutputFile, Task};

    const PINNED_IMAGE: &str =
        "image@sha256:0000000000000000000000000000000000000000000000000000000000000000";

    #[derive(Default)]
    struct FakeFiles {
        locations: HashMap<String, PathBuf>,
    }

    impl Files for FakeFiles {
        fn local(&self, location: &str) -> io::Result<PathBuf> {
            self.locations.get(location).cloned().ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, format!("no file at {location}"))
            })
        }

        fn deliver(&self, _delivery: &Delivery) -> io::Result<()> {
            Ok(())
        }
    }

    struct Fresh {
        calls: Arc<AtomicUsize>,
        files: Arc<dyn Files>,
    }

    impl Executor for Fresh {
        fn run(&mut self, attempt: Attempt) -> Running {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Running::ended(Ok(Outcome {
                id: attempt.task.id,
                ..Outcome::default()
            }))
        }

        fn files(&self) -> Option<Arc<dyn Files>> {
            Some(self.files.clone())
        }

        fn release(&mut self) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {})
        }
    }

    fn fresh(calls: Arc<AtomicUsize>, files: Arc<dyn Files>) -> Box<dyn Executor> {
        Box::new(Fresh { calls, files })
    }

    fn attempt(task: Arc<Task>) -> Attempt {
        Attempt {
            task,
            number: 2,
            cancel: CancellationToken::new(),
        }
    }

    fn real_task() -> Task {
        Task {
            id: TaskId(1),
            image: Some(PINNED_IMAGE.into()),
            command: vec!["true".into()],
            network: Network::Allowed,
            ..Task::default()
        }
    }

    fn history(task: &Task, outcome: Outcome) -> HashMap<String, Reusable> {
        [(
            fingerprint(task, &input_states(task)),
            Reusable {
                run: "prior".into(),
                outcome,
            },
        )]
        .into()
    }

    #[test]
    fn forwards_daemon_release() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut resumed = Resumed::new(
            fresh(calls.clone(), Arc::new(FakeFiles::default())),
            HashMap::new(),
            Taken::default(),
            Mode::Simulated,
            NetworkRule::default(),
            false,
        );
        workflow_graph::runtime()
            .unwrap()
            .block_on(resumed.release());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn respects_reuse_policy() {
        let runtime = workflow_graph::runtime().unwrap();
        for policy in [Reuse::Allowed, Reuse::Denied, Reuse::Disabled] {
            let task = Arc::new(Task {
                id: TaskId(1),
                reuse: policy,
                ..Task::default()
            });
            let prior = Reusable {
                run: "prior".into(),
                outcome: Outcome::default(),
            };
            let history = [(fingerprint(&task, &input_states(&task)), prior)].into();
            let calls = Arc::new(AtomicUsize::new(0));
            let taken = Taken::default();
            let mut resumed = Resumed::new(
                fresh(calls.clone(), Arc::new(FakeFiles::default())),
                history,
                taken.clone(),
                Mode::Simulated,
                NetworkRule::default(),
                false,
            );
            let outcome = runtime.block_on(resumed.run(attempt(task))).unwrap();
            let reused = policy == Reuse::Allowed;
            assert_eq!(outcome.cached, reused);
            assert_eq!(calls.load(Ordering::SeqCst), usize::from(!reused));
            assert_eq!(taken.take(TaskId(1)).is_some(), reused);
        }
    }

    #[test]
    fn verifies_remote_outputs() {
        let runtime = workflow_graph::runtime().unwrap();
        let folder = tempfile::tempdir().expect("a temporary directory");
        let output = folder.path().join("output.txt");
        std::fs::write(&output, "old").expect("writes the output");
        let task = Arc::new(real_task());
        let prior = Outcome {
            image: task.image.clone(),
            accepted: Some(true),
            files: vec![OutputFile {
                path: "output.txt".into(),
                location: "memory://output".into(),
                directory: false,
                size: Some(3),
                digest: Some(Digest::sha256(b"old").to_string()),
            }],
            ..Outcome::default()
        };
        let provider = Arc::new(FakeFiles {
            locations: [("memory://output".into(), output.clone())].into(),
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let mut resumed = Resumed::new(
            fresh(calls.clone(), provider.clone()),
            history(&task, prior.clone()),
            Taken::default(),
            Mode::Real,
            NetworkRule::default(),
            false,
        );
        let found = runtime
            .block_on(resumed.run(attempt(task.clone())))
            .unwrap();
        assert!(found.cached);
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        std::fs::write(&output, "new").expect("mutates the output without changing its size");
        let mut resumed = Resumed::new(
            fresh(calls.clone(), provider),
            history(&task, prior.clone()),
            Taken::default(),
            Mode::Real,
            NetworkRule::default(),
            false,
        );
        let found = runtime.block_on(resumed.run(attempt(task))).unwrap();
        assert!(!found.cached);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        std::fs::remove_file(&output).expect("removes the changed output");
        std::fs::create_dir(&output).expect("replaces the output with a directory");
        let task = Arc::new(real_task());
        let mut resumed = Resumed::new(
            fresh(
                calls.clone(),
                Arc::new(FakeFiles {
                    locations: [("memory://output".into(), output)].into(),
                }),
            ),
            history(
                &task,
                Outcome {
                    image: task.image.clone(),
                    accepted: Some(true),
                    files: prior.files,
                    ..Outcome::default()
                },
            ),
            Taken::default(),
            Mode::Real,
            NetworkRule::default(),
            false,
        );
        let found = runtime.block_on(resumed.run(attempt(task))).unwrap();
        assert!(!found.cached);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn verifies_input_bytes() {
        let runtime = workflow_graph::runtime().unwrap();
        let folder = tempfile::tempdir().expect("a temporary directory");
        let input = folder.path().join("input.txt");
        std::fs::write(&input, "old").expect("writes the input");
        let modified = std::fs::metadata(&input)
            .expect("input metadata")
            .modified()
            .expect("input modification time");
        let mut task = real_task();
        task.inputs.push(Input {
            path: "input.txt".into(),
            source: input.to_string_lossy().into_owned(),
            ..Input::default()
        });
        let old_states = input_states(&task);
        let prior = Outcome {
            image: task.image.clone(),
            accepted: Some(true),
            inputs: [("input.txt".into(), Digest::sha256(b"old").to_string())].into(),
            ..Outcome::default()
        };
        let history = [(
            fingerprint(&task, &old_states),
            Reusable {
                run: "prior".into(),
                outcome: prior,
            },
        )]
        .into();
        std::fs::write(&input, "new").expect("mutates the input without changing its size");
        std::fs::File::options()
            .write(true)
            .open(&input)
            .expect("opens the input")
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .expect("restores the modification time");
        assert_eq!(input_states(&task), old_states);

        let calls = Arc::new(AtomicUsize::new(0));
        let mut resumed = Resumed::new(
            fresh(calls.clone(), Arc::new(FakeFiles::default())),
            history,
            Taken::default(),
            Mode::Real,
            NetworkRule::default(),
            false,
        );
        let found = runtime
            .block_on(resumed.run(attempt(Arc::new(task))))
            .unwrap();
        assert!(!found.cached);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// A Snakemake job of a shared folder resumes by the path of the folder and the bytes of
    /// the declared inputs in it.
    #[test]
    fn resumes_shared_folders() {
        let runtime = workflow_graph::runtime().unwrap();
        let folder = tempfile::tempdir().expect("a temporary directory");
        let input = folder.path().join("in.txt");
        std::fs::write(&input, "in").expect("writes the input");
        let place = folder.path().canonicalize().expect("a path");
        let path = place.to_string_lossy().into_owned();
        let mut task = real_task();
        task.inputs.push(Input {
            path: path.clone(),
            source: path.clone(),
            directory: true,
            writable: Writable::Shared,
            ..Input::default()
        });
        task.inputs.push(Input {
            path: "in.txt".into(),
            source: input.to_string_lossy().into_owned(),
            ..Input::default()
        });
        let prior = Outcome {
            image: task.image.clone(),
            accepted: Some(true),
            inputs: [
                (path.clone(), Digest::sha256(path.as_bytes()).to_string()),
                ("in.txt".into(), Digest::sha256(b"in").to_string()),
            ]
            .into(),
            ..Outcome::default()
        };
        let history = [(
            fingerprint(&task, &input_states(&task)),
            Reusable {
                run: "prior".into(),
                outcome: prior,
            },
        )]
        .into();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut resumed = Resumed::new(
            fresh(calls.clone(), Arc::new(FakeFiles::default())),
            history,
            Taken::default(),
            Mode::Real,
            NetworkRule::default(),
            false,
        );
        let found = runtime
            .block_on(resumed.run(attempt(Arc::new(task))))
            .unwrap();
        assert!(found.cached);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn rejects_unaccepted_results() {
        let runtime = workflow_graph::runtime().unwrap();
        for accepted in [None, Some(false), Some(true)] {
            let task = Arc::new(real_task());
            let prior = Outcome {
                image: task.image.clone(),
                accepted,
                ..Outcome::default()
            };
            let calls = Arc::new(AtomicUsize::new(0));
            let mut resumed = Resumed::new(
                fresh(calls.clone(), Arc::new(FakeFiles::default())),
                history(&task, prior),
                Taken::default(),
                Mode::Real,
                NetworkRule::default(),
                false,
            );
            let outcome = runtime.block_on(resumed.run(attempt(task))).unwrap();
            assert_eq!(outcome.cached, accepted == Some(true));
            assert_eq!(
                calls.load(Ordering::SeqCst),
                usize::from(accepted != Some(true))
            );
        }
    }

    #[test]
    fn unpinned_images_defer() {
        let runtime = workflow_graph::runtime().unwrap();
        let mut task = real_task();
        task.image = Some("image@sha256:abc".into());
        let prior = Outcome {
            image: Some(PINNED_IMAGE.into()),
            accepted: Some(true),
            ..Outcome::default()
        };
        let history = history(&task, prior);
        let calls = Arc::new(AtomicUsize::new(0));
        let mut resumed = Resumed::new(
            fresh(calls.clone(), Arc::new(FakeFiles::default())),
            history,
            Taken::default(),
            Mode::Real,
            NetworkRule::default(),
            false,
        );

        let found = runtime
            .block_on(resumed.run(attempt(Arc::new(task))))
            .unwrap();
        assert!(!found.cached);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn resumes_unstated_network() {
        let runtime = workflow_graph::runtime().unwrap();
        let mut task = real_task();
        task.network = Network::Unstated;
        let prior = Outcome {
            image: task.image.clone(),
            accepted: Some(true),
            ..Outcome::default()
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let mut resumed = Resumed::new(
            fresh(calls.clone(), Arc::new(FakeFiles::default())),
            history(&task, prior),
            Taken::default(),
            Mode::Real,
            NetworkRule::default(),
            false,
        );
        let found = runtime
            .block_on(resumed.run(attempt(Arc::new(task))))
            .unwrap();
        assert!(
            found.cached,
            "a Nextflow or Snakemake task states no network"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn network_changes_defer() {
        let runtime = workflow_graph::runtime().unwrap();
        let cases = [
            (
                Network::Unstated,
                "ordinary",
                NetworkRule {
                    none_by_default: true,
                    ..NetworkRule::default()
                },
            ),
            (
                Network::Denied,
                "fetch",
                NetworkRule {
                    allowed_steps: vec!["fetch".into()],
                    ..NetworkRule::default()
                },
            ),
        ];
        for (network, step, rule) in cases {
            let mut task = real_task();
            task.network = network;
            task.step = step.into();
            let prior = Outcome {
                image: task.image.clone(),
                accepted: Some(true),
                ..Outcome::default()
            };
            let history = history(&task, prior);
            let calls = Arc::new(AtomicUsize::new(0));
            let mut resumed = Resumed::new(
                fresh(calls.clone(), Arc::new(FakeFiles::default())),
                history,
                Taken::default(),
                Mode::Real,
                rule,
                false,
            );

            let found = runtime
                .block_on(resumed.run(attempt(Arc::new(task))))
                .unwrap();
            assert!(!found.cached);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
    }
}
