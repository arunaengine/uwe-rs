//! What every real executor checks before it starts a task. A task that does nothing finishes
//! at once, and a task without a container image gets the default image or is an error, since
//! every task runs in a container (decision 11). The simulator needs neither check: it
//! pretends every task, so that a simulated run still gets the files its driver expects.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::Arc;

use workflow_graph::{Attempt, ExecuteError, Executor, Outcome, Running, Task};

/// A real executor behind the checks.
pub struct Checked<E> {
    executor: E,
    /// The image of tasks that name none, from `-default-image`.
    default_image: Option<String>,
}

impl<E: Executor> Checked<E> {
    pub fn new(executor: E, default_image: Option<String>) -> Self {
        Checked {
            executor,
            default_image,
        }
    }
}

impl<E: Executor> Executor for Checked<E> {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = &attempt.task;
        if task.is_no_op() {
            return Running::ended(Ok(Outcome {
                id: task.id,
                ..Outcome::default()
            }));
        }
        let task = match (&task.image, &self.default_image) {
            (Some(_), _) => task.clone(),
            (None, Some(image)) => Arc::new(Task {
                image: Some(image.clone()),
                ..Task::clone(task)
            }),
            (None, None) => {
                return Running::ended(Err(ExecuteError::Executor {
                    task: task.name.clone(),
                    message: "the task names no container image; iunctus runs every task in a \
                              container, so name an image in the workflow or give one with \
                              `-default-image <image>`"
                        .into(),
                }));
            }
        };
        self.executor.run(Attempt { task, ..attempt })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Notes the image of each task it gets.
    #[derive(Default)]
    struct Images(Vec<Option<String>>);

    impl Executor for Images {
        fn run(&mut self, attempt: Attempt) -> Running {
            self.0.push(attempt.task.image.clone());
            Running::ended(Ok(Outcome {
                id: attempt.task.id,
                exit_code: 3,
                ..Outcome::default()
            }))
        }
    }

    /// Runs the first attempt of a task and waits for its outcome.
    fn execute(checked: &mut Checked<Images>, task: Task) -> Result<Outcome, ExecuteError> {
        let attempt = Attempt {
            task: Arc::new(task),
            number: 1,
            cancel: Default::default(),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime");
        runtime.block_on(checked.run(attempt))
    }

    fn task(image: Option<&str>, command: &[&str]) -> Task {
        Task {
            name: "t".into(),
            image: image.map(str::to_owned),
            command: command.iter().map(|word| (*word).to_owned()).collect(),
            outputs: vec!["out.txt".into()],
            ..Task::default()
        }
    }

    #[test]
    fn tasks_without_a_command_or_an_image_do_nothing() {
        let mut checked = Checked::new(Images::default(), None);
        let outcome = execute(&mut checked, task(None, &[])).expect("an outcome");
        assert_eq!((outcome.exit_code, outcome.files.len()), (0, 0));
        // An image without a command runs the image's default command.
        let outcome = execute(&mut checked, task(Some("debian"), &[])).expect("runs");
        assert_eq!(outcome.exit_code, 3);
        assert_eq!(checked.executor.0, [Some("debian".to_owned())]);
    }

    #[test]
    fn tasks_need_an_image_or_the_default_one() {
        let mut checked = Checked::new(Images::default(), None);
        let error = execute(&mut checked, task(None, &["true"])).unwrap_err();
        assert!(
            error.to_string().contains("names no container image"),
            "{error}"
        );
        let mut checked = Checked::new(Images::default(), Some("debian".into()));
        execute(&mut checked, task(None, &["true"])).expect("runs");
        execute(&mut checked, task(Some("alpine"), &["true"])).expect("runs");
        let images = [Some("debian".to_owned()), Some("alpine".to_owned())];
        assert_eq!(checked.executor.0, images);
    }
}
