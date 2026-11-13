//! What a Nextflow run does when a task fails, by its `errorStrategy`: retry it with its
//! attempt counted up, ignore it, finish the running tasks first, or stop at once.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use workflow_graph::{
    Attempt, Engine, ExecuteError, Executor, Launch, Order, Outcome, OutputFile, RunOptions,
    Running,
};

/// The step, the attempt and the memory in GiB of an attempt.
type Ran = (String, u32, Option<u64>);

/// Fails the attempts it is told to with their exit codes, and notes each attempt.
#[derive(Default)]
struct Scripted {
    exit_codes: HashMap<(String, u32), i32>,
    ran: Vec<Ran>,
}

impl Executor for Scripted {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = &attempt.task;
        let step = task.step.rsplit(':').next().unwrap_or_default().to_owned();
        let memory = task.resources.memory_bytes.map(|bytes| bytes >> 30);
        self.ran.push((step.clone(), attempt.number, memory));
        let code = self.exit_codes.get(&(step, attempt.number)).copied();
        Running::ended(Ok(Outcome {
            id: task.id,
            exit_code: code.unwrap_or(0),
            ..Outcome::default()
        }))
    }
}

const PIPELINE: &str = r#"
process FIRST {
    input:
    val x
    output:
    val x
    script:
    "echo ${x}"
}

process SECOND {
    input:
    val x
    script:
    "echo ${x}"
}

workflow {
    SECOND(FIRST(channel.of(1)))
}
"#;

/// Runs the pipeline with this config and these failures; the result has the error and what
/// ran.
fn run(config: &str, failures: &[(&str, u32, i32)]) -> (Option<String>, Vec<Ran>) {
    run_pipeline(PIPELINE, config, failures, false)
}

/// Runs a pipeline with this config and these failures, and keeps going after a failure when
/// asked to.
fn run_pipeline(
    pipeline: &str,
    config: &str,
    failures: &[(&str, u32, i32)],
    keep_going: bool,
) -> (Option<String>, Vec<Ran>) {
    let directory = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(directory.path().join("main.nf"), pipeline).expect("writes the script");
    std::fs::write(directory.path().join("nextflow.config"), config).expect("writes the config");
    let launch = Launch {
        target: directory.path().to_owned(),
        keep_going,
        ..Launch::default()
    };
    let mut driver = nextflow_runtime::driver(&launch).expect("a driver");
    let mut executor = Scripted {
        exit_codes: (failures.iter())
            .map(|(step, attempt, code)| (((*step).to_owned(), *attempt), *code))
            .collect(),
        ..Scripted::default()
    };
    let options = RunOptions {
        max_pending: 4,
        order: Order::AsAsked,
    };
    let execution = Engine::new(&mut executor)
        .options(options)
        .run("main.nf", driver.as_mut());
    let error = execution.error.map(|error: ExecuteError| error.to_string());
    (error, executor.ran)
}

const RETRY: &str = r#"
process {
    errorStrategy = { task.exitStatus in 137..140 ? 'retry' : 'terminate' }
    maxRetries = 2
    memory = { 1.GB * task.attempt }
}
"#;

#[test]
fn retries_increased_memory() {
    let (error, ran) = run(RETRY, &[("FIRST", 1, 137), ("FIRST", 2, 137)]);
    assert_eq!(error, None);
    let expected = [
        ("FIRST".to_owned(), 1, Some(1)),
        ("FIRST".to_owned(), 2, Some(2)),
        ("FIRST".to_owned(), 3, Some(3)),
        ("SECOND".to_owned(), 1, Some(1)),
    ];
    assert_eq!(ran, expected);
    // Out of retries, or with an exit code the closure does not retry, the run stops.
    let (error, _) = run(
        RETRY,
        &[("FIRST", 1, 137), ("FIRST", 2, 137), ("FIRST", 3, 137)],
    );
    let error = error.expect("an error");
    assert!(
        error.contains("terminated with an error exit status (137)"),
        "{error}"
    );
    let (error, ran) = run(RETRY, &[("FIRST", 1, 1)]);
    assert!(error.is_some());
    assert_eq!(ran.len(), 1);
}

#[test]
fn empties_ignored_outputs() {
    let config = "process.errorStrategy = 'ignore'\n";
    let (error, ran) = run(config, &[("FIRST", 1, 2)]);
    assert_eq!(error, None);
    assert_eq!(ran, [("FIRST".to_owned(), 1, None)]);
}

#[test]
fn drains_finishing_tasks() {
    let config = "process.errorStrategy = 'finish'\n";
    let (error, ran) = run(config, &[("FIRST", 1, 3)]);
    let error = error.expect("an error");
    assert!(
        error.contains("terminated with an error exit status (3)"),
        "{error}"
    );
    assert_eq!(ran, [("FIRST".to_owned(), 1, None)]);
}

/// A failing process and an independent branch of two processes.
const BRANCHES: &str = r#"
process FAIL {
    script:
    "exit 3"
}

process SLOW {
    output:
    val 1
    script:
    "sleep 6"
}

process LATER {
    input:
    val x
    script:
    "echo ${x}"
}

workflow {
    FAIL()
    LATER(SLOW())
}
"#;

/// The steps that ran, in order.
fn steps(ran: &[Ran]) -> Vec<&str> {
    ran.iter().map(|(step, _, _)| step.as_str()).collect()
}

#[test]
fn stops_failed_admission() {
    let failure = [("FAIL", 1, 3)];
    // Nextflow 26.04.6 with `terminate`, the default, stops at once: the running `SLOW` is
    // killed, and `LATER` never starts. With `finish`, `SLOW` ends first, and `LATER` never
    // starts either (recorded with a probe of both strategies).
    for config in ["", "process.errorStrategy = 'finish'\n"] {
        let (error, ran) = run_pipeline(BRANCHES, config, &failure, false);
        let error = error.expect("an error");
        assert!(error.contains("`FAIL` terminated"), "{error}");
        assert_eq!(steps(&ran), ["FAIL", "SLOW"], "{config}");
    }
    // `-keep-going` runs what does not depend on the failed task, then fails the run.
    let (error, ran) = run_pipeline(BRANCHES, "", &failure, true);
    let error = error.expect("an error");
    assert!(error.contains("`FAIL` terminated"), "{error}");
    assert_eq!(steps(&ran), ["FAIL", "SLOW", "LATER"]);
}

#[test]
fn retries_rejected_cache() {
    struct Cached {
        fresh: usize,
        policies: Vec<workflow_graph::Reuse>,
    }
    impl Executor for Cached {
        fn run(&mut self, attempt: Attempt) -> Running {
            let task = attempt.task;
            self.policies.push(task.reuse);
            if task.reuse == workflow_graph::Reuse::Allowed {
                return Running::ended(Ok(Outcome {
                    id: task.id,
                    cached: true,
                    ..Outcome::default()
                }));
            }
            self.fresh += 1;
            Running::ended(Ok(Outcome {
                id: task.id,
                files: vec![OutputFile {
                    path: "out.txt".into(),
                    location: "/work/out.txt".into(),
                    ..OutputFile::default()
                }],
                ..Outcome::default()
            }))
        }
    }
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("main.nf"), "process P {\n output:\n path 'out.txt'\n script:\n 'touch out.txt'\n}\nworkflow {\n P()\n}\n").unwrap();
    std::fs::write(
        directory.path().join("nextflow.config"),
        "process.errorStrategy = 'retry'\n",
    )
    .unwrap();
    let launch = Launch {
        target: directory.path().to_owned(),
        ..Launch::default()
    };
    let mut driver = nextflow_runtime::driver(&launch).unwrap();
    let mut executor = Cached {
        fresh: 0,
        policies: Vec::new(),
    };
    let execution = Engine::new(&mut executor).run("main.nf", driver.as_mut());
    assert!(execution.error.is_none(), "{:?}", execution.error);
    assert_eq!(executor.fresh, 1);
    assert_eq!(
        executor.policies,
        [
            workflow_graph::Reuse::Allowed,
            workflow_graph::Reuse::Denied
        ]
    );
}

#[test]
fn applies_collection_policy() {
    use nextflow_evaluator::interpreter::Host;
    use std::cell::RefCell;
    use std::rc::Rc;

    struct Printed(Rc<RefCell<String>>);
    impl Host for Printed {
        fn print(&mut self, text: &str) {
            self.0.borrow_mut().push_str(text);
        }
    }
    struct Collecting {
        directory: std::path::PathBuf,
        eval: bool,
        unreadable: bool,
        metadata: usize,
        recover: bool,
        ran: Vec<(String, u32)>,
    }
    impl Executor for Collecting {
        fn run(&mut self, attempt: Attempt) -> Running {
            let task = attempt.task;
            self.ran.push((task.step.clone(), attempt.number));
            let mut files = Vec::new();
            if task.step == "FAIL" {
                let fail = !self.recover || attempt.number == 1;
                if self.eval {
                    let location = self.directory.join(format!("eval-{}", attempt.number));
                    let status = if fail { 2 } else { 0 };
                    if !(self.unreadable && fail) {
                        let record = if fail && self.metadata == 4 {
                            "nxf_out_eval_1=answer\n".into()
                        } else if fail && self.metadata == 5 {
                            "nxf_out_eval_1=answer\n/nxf_out_eval_1/\n".into()
                        } else {
                            format!("nxf_out_eval_1=answer\n/nxf_out_eval_1/=exit:{status}\n")
                        };
                        std::fs::write(&location, record).unwrap();
                    }
                    if !(fail && self.metadata == 3) {
                        files.push(OutputFile {
                            path: ".command.env".into(),
                            location: location.to_string_lossy().into_owned(),
                            ..OutputFile::default()
                        });
                    }
                } else if !fail {
                    let path = self.directory.join("out.txt");
                    let location = path.to_string_lossy().into_owned();
                    files.push(OutputFile {
                        path: "out.txt".into(),
                        location,
                        ..OutputFile::default()
                    });
                }
            }
            Running::ended(Ok(Outcome {
                id: task.id,
                exit_code: if task.step == "FAIL"
                    && self.metadata == 7
                    && (!self.recover || attempt.number == 1)
                {
                    3
                } else {
                    0
                },
                files,
                ..Outcome::default()
            }))
        }
    }
    for mode in 0..8 {
        let eval = mode != 0 && mode != 6;
        let unreadable = mode == 2;
        for (strategy, keep_going, recover, expected) in [
            ("retry", false, true, vec!["FAIL", "SLOW", "FAIL", "LATER"]),
            ("retry", false, false, vec!["FAIL", "SLOW", "FAIL", "LATER"]),
            ("ignore", false, false, vec!["FAIL", "SLOW", "LATER"]),
            ("finish", false, false, vec!["FAIL", "SLOW"]),
            ("terminate", true, false, vec!["FAIL", "SLOW", "LATER"]),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let output = if eval {
                "eval 'false'"
            } else if mode == 6 {
                "path 'out.txt', arity: '1', optional: true"
            } else {
                "path 'out.txt'"
            };
            let pipeline = BRANCHES.replace(
                "    script:\n    \"exit 3\"",
                &format!("    output:\n    val 99, emit: early\n    {output}, emit: checked\n    script:\n    'true'"),
            ).replace("    FAIL()", "    FAIL()\n    FAIL.out.early.view { \"accepted=${it}\" }\n    workflow.onComplete { println(\"successes=${workflow.stats.succeededCount}\") }");
            std::fs::write(directory.path().join("main.nf"), pipeline).unwrap();
            std::fs::write(
                directory.path().join("nextflow.config"),
                format!("process.errorStrategy = '{strategy}'\n"),
            )
            .unwrap();
            let launch = Launch {
                target: directory.path().to_owned(),
                keep_going,
                ..Launch::default()
            };
            let printed = Rc::new(RefCell::new(String::new()));
            let mut driver = nextflow_runtime::driver::Nextflow::new(
                &launch,
                nextflow_runtime::driver::Settings {
                    host: Box::new(Printed(printed.clone())),
                    ..nextflow_runtime::driver::Settings::default()
                },
            )
            .unwrap();
            let mut executor = Collecting {
                directory: directory.path().to_owned(),
                eval,
                unreadable,
                metadata: mode,
                recover,
                ran: Vec::new(),
            };
            let execution = Engine::new(&mut executor)
                .options(RunOptions {
                    max_pending: 4,
                    order: Order::AsAsked,
                })
                .run("main.nf", &mut driver);
            let error = execution.error.map(|error| error.to_string());
            assert_eq!(
                error.is_none(),
                recover || strategy == "ignore",
                "{strategy}, eval={eval}: {error:?}"
            );
            if let Some(error) = error {
                assert!(
                    error.contains(if mode == 7 {
                        "exit status (3)"
                    } else if unreadable {
                        "cannot read"
                    } else if mode == 6 {
                        "Output arity"
                    } else if mode >= 3 {
                        "missing capture"
                    } else if eval {
                        "eval` output command failed"
                    } else {
                        "Missing output file"
                    }),
                    "{error}"
                );
            }
            let steps: Vec<_> = executor.ran.iter().map(|(step, _)| step.as_str()).collect();
            assert_eq!(steps, expected, "{strategy}, eval={eval}");
            assert_eq!(
                printed.borrow().matches("accepted=99\n").count(),
                usize::from(recover)
            );
            if recover || strategy == "ignore" {
                let count = if recover { 3 } else { 2 };
                assert!(
                    printed.borrow().contains(&format!("successes={count}\n")),
                    "{}",
                    printed.borrow()
                );
            }
            if strategy == "retry" {
                let attempts: Vec<_> = executor
                    .ran
                    .iter()
                    .filter(|(step, _)| step == "FAIL")
                    .map(|(_, attempt)| *attempt)
                    .collect();
                assert_eq!(attempts, [1, 2]);
            }
        }
    }
}
