//! Workflow terminal metadata and one-shot callbacks through the engine.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nextflow_evaluator::interpreter::Host;
use nextflow_runtime::driver::{Nextflow, Settings};
use workflow_graph::{
    Attempt, Driver, Engine, Executor, Launch, Order, Outcome, RunOptions, Running,
};

struct Printed {
    text: Rc<RefCell<String>>,
    cleaned: Arc<AtomicBool>,
}

impl Host for Printed {
    fn print(&mut self, text: &str) {
        if text.starts_with("hook-error ") || text.starts_with("hook-complete ") {
            assert!(
                self.cleaned.load(Ordering::SeqCst),
                "callback before cleanup"
            );
        }
        self.text.borrow_mut().push_str(text);
    }
}

struct Scripted {
    mode: u8,
    cleaned: Arc<AtomicBool>,
}

impl Executor for Scripted {
    fn run(&mut self, attempt: Attempt) -> Running {
        let id = attempt.task.id;
        let slow = attempt.task.step == "Q" && matches!(self.mode, 1 | 3);
        if self.mode == 2 || slow {
            let cancel = attempt.cancel;
            if self.mode == 2 {
                cancel.cancel();
            }
            let cleaned = self.cleaned.clone();
            return Running::new(async move {
                cancel.cancelled().await;
                cleaned.store(true, Ordering::SeqCst);
                Ok(Outcome {
                    id,
                    exit_code: 143,
                    ..Outcome::default()
                })
            });
        }
        Running::ended(Ok(Outcome {
            id,
            exit_code: if self.mode == 1 { 3 } else { 0 },
            ..Outcome::default()
        }))
    }
}

#[test]
fn preserves_callback_failure() {
    for (mode, error_hook_fails, completion_fails, status, primary) in [
        (0, false, false, 0, ""),
        (1, false, false, 3, "exit status (3)"),
        (2, false, false, 130, "the run was stopped"),
        (3, false, false, 0, "Missing output file"),
        (4, false, false, 1, "start failure"),
        (1, true, true, 3, "exit status (3)"),
        (0, false, true, 1, "completion failure"),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let output = if mode == 3 {
            "output:\n path 'missing.txt'\n"
        } else {
            ""
        };
        let body = if mode == 4 {
            "error('start failure')"
        } else {
            "P()\n Q()"
        };
        let error_hook = if error_hook_fails {
            "error('error hook failure')"
        } else {
            ""
        };
        let completion_hook = if completion_fails {
            "error('completion failure')"
        } else {
            ""
        };
        let source = format!(
            "process P {{\n {output}script:\n 'true'\n}}\nprocess Q {{\n script:\n 'true'\n}}\nworkflow {{\n workflow.onError {{ trace ->\n  println(\"hook-error ${{workflow.success}} ${{workflow.exitStatus}} ${{trace?.exit}}\")\n  assert workflow.complete != null\n  assert workflow.duration != null\n  assert workflow.errorMessage == workflow.errorReport\n  println(workflow.errorMessage)\n  {error_hook}\n }}\n workflow.onComplete {{\n  println(\"hook-complete ${{workflow.success}} ${{workflow.exitStatus}}\")\n  assert workflow.complete != null\n  assert workflow.duration != null\n  {completion_hook}\n }}\n {body}\n}}\n"
        );
        std::fs::write(directory.path().join("main.nf"), source).unwrap();
        let launch = Launch {
            target: directory.path().to_owned(),
            ..Launch::default()
        };
        let text = Rc::new(RefCell::new(String::new()));
        let cleaned = Arc::new(AtomicBool::new(!matches!(mode, 1..=3)));
        let mut driver = Nextflow::new(
            &launch,
            Settings {
                host: Box::new(Printed {
                    text: text.clone(),
                    cleaned: cleaned.clone(),
                }),
                ..Settings::default()
            },
        )
        .unwrap();
        let mut executor = Scripted { mode, cleaned };
        let execution = Engine::new(&mut executor)
            .options(RunOptions {
                max_pending: 4,
                order: Order::AsAsked,
            })
            .run("main.nf", &mut driver);
        let printed = text.borrow().clone();
        assert_eq!(
            printed
                .lines()
                .filter(|line| line.starts_with("hook-complete "))
                .count(),
            1,
            "{printed}"
        );
        if primary.is_empty() {
            assert!(execution.error.is_none(), "{:?}", execution.error);
            assert!(!printed.contains("hook-error "), "{printed}");
            assert!(printed.contains("hook-complete true 0\n"), "{printed}");
            driver.finish().unwrap();
        } else {
            let error = execution.error.as_ref().unwrap();
            assert!(error.to_string().contains(primary), "{error}");
            assert_eq!(
                printed
                    .lines()
                    .filter(|line| line.starts_with("hook-error "))
                    .count(),
                1,
                "{printed}"
            );
            let trace_exit = if matches!(mode, 1 | 3) {
                status.to_string()
            } else {
                "null".into()
            };
            assert!(
                printed.contains(&format!("hook-error false {status} {trace_exit}\n")),
                "{printed}"
            );
            assert!(printed.contains(primary), "{printed}");
            if error_hook_fails {
                assert!(
                    execution
                        .failure_hook_error
                        .as_ref()
                        .unwrap()
                        .message
                        .contains("error hook failure")
                );
                assert!(printed.contains("hook-complete false 3\n"), "{printed}");
            } else {
                assert!(
                    execution.failure_hook_error.is_none(),
                    "{:?}",
                    execution.failure_hook_error
                );
            }
            driver.failed(error).unwrap();
        }
        assert_eq!(*text.borrow(), printed, "repeated callback invocation");
    }
}
