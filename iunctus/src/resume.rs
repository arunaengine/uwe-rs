//! Resume from the run database: a run that continues an earlier one takes the result of each
//! task that finished there, instead of running it again. A task counts as the same when its
//! fingerprint is the same: the same description and the same size and time of each local
//! input. This works for every executor, simulated or real; the store's cache still answers
//! tasks whose image is pinned, in any run.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use run_database::{Recorder, Reusable, fingerprint, input_states};
use workflow_graph::{Attempt, Event, Executor, Files, Observer, Outcome, Reuse, Running, TaskId};

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
    /// Prints one line per task taken from an earlier run when set.
    verbose: bool,
}

impl Resumed {
    pub fn new(
        executor: Box<dyn Executor>,
        reusable: HashMap<String, Reusable>,
        taken: Taken,
        verbose: bool,
    ) -> Self {
        Resumed {
            executor,
            reusable,
            taken,
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
        // A result whose files are gone cannot be taken.
        let found = self.reusable.get(&key).filter(|found| {
            let files = &found.outcome.files;
            files.iter().all(|file| Path::new(&file.location).exists())
        });
        let Some(found) = found else {
            return self.executor.run(attempt);
        };
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use workflow_graph::{CancellationToken, Task};

    struct Fresh(Arc<AtomicUsize>);

    impl Executor for Fresh {
        fn run(&mut self, attempt: Attempt) -> Running {
            self.0.fetch_add(1, Ordering::SeqCst);
            Running::ended(Ok(Outcome {
                id: attempt.task.id,
                ..Outcome::default()
            }))
        }
    }

    #[test]
    fn denied_and_disabled_tasks_bypass_matching_lineage_results() {
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
                Box::new(Fresh(calls.clone())),
                history,
                taken.clone(),
                false,
            );
            let outcome = runtime
                .block_on(resumed.run(Attempt {
                    task,
                    number: 2,
                    cancel: CancellationToken::new(),
                }))
                .unwrap();
            let reused = policy == Reuse::Allowed;
            assert_eq!(outcome.cached, reused);
            assert_eq!(calls.load(Ordering::SeqCst), usize::from(!reused));
            assert_eq!(taken.take(TaskId(1)).is_some(), reused);
        }
    }
}
