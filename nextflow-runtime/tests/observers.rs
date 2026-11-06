//! Run observers hear the events of a real run in order: its start, each task, the files it
//! publishes and its end, with the trace records of the tasks.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use nextflow_evaluator::interpreter::Host;
use nextflow_plugins::observer::{Counts, Event, Observer, Record};
use nextflow_runtime::driver::{Nextflow, Settings};
use workflow_graph::{
    Attempt, Delivery, Engine, Executor, Files, Launch, Outcome, OutputFile, Running,
};

const PIPELINE: &str = r#"
process FIRST {
    publishDir 'results'
    output:
    path 'out.txt'
    script:
    "echo a > out.txt"
}

process SECOND {
    input:
    path x
    script:
    "cat ${x}"
}

workflow {
    SECOND(FIRST())
}
"#;

/// Keeps every event it hears.
struct Recorder(Rc<RefCell<Vec<Event>>>);

impl Observer for Recorder {
    fn notify(&mut self, event: &Event) {
        self.0.borrow_mut().push(event.clone());
    }
}

/// Keeps what the run prints.
struct Printed(Rc<RefCell<String>>);

impl Host for Printed {
    fn print(&mut self, text: &str) {
        self.0.borrow_mut().push_str(text);
    }
}

/// Answers FIRST from the cache, as on resume, and fails SECOND.
struct Resumed;

impl Executor for Resumed {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = attempt.task;
        let outcome = match task.step.ends_with("FIRST") {
            true => Outcome {
                id: task.id,
                cached: true,
                files: vec![OutputFile {
                    path: "out.txt".into(),
                    location: "/work/out.txt".into(),
                    ..OutputFile::default()
                }],
                ..Outcome::default()
            },
            false => Outcome {
                id: task.id,
                exit_code: 1,
                ..Outcome::default()
            },
        };
        Running::ended(Ok(outcome))
    }

    fn files(&self) -> Option<Arc<dyn Files>> {
        Some(Arc::new(Kept))
    }
}

/// Files that are where they are; delivering them does nothing.
struct Kept;

impl Files for Kept {
    fn local(&self, location: &str) -> std::io::Result<PathBuf> {
        Ok(PathBuf::from(location))
    }

    fn deliver(&self, _: &Delivery) -> std::io::Result<()> {
        Ok(())
    }
}

/// The kind of an event, with the task or file it concerns.
fn summary(event: &Event) -> String {
    let name = |record: &Record| record["name"].as_str().unwrap_or_default().to_owned();
    match event {
        Event::RunStarted(_) => "run started".into(),
        Event::TaskSubmitted(record) => format!("submitted {}", name(record)),
        Event::TaskStarted(record) => format!("started {}", name(record)),
        Event::TaskCompleted(record) => format!("completed {}", name(record)),
        Event::TaskCached(record) => format!("cached {}", name(record)),
        Event::TaskFailed(record) => format!("failed {}", name(record)),
        Event::FilePublished { source, target } => {
            let target = target.rsplit("results/").next().unwrap_or_default();
            format!("published {source} as {target}")
        }
        Event::RunEnded(_) => "run ended".into(),
    }
}

#[test]
fn hears_run_events() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(directory.path().join("main.nf"), PIPELINE).expect("writes the script");
    let config = "plugins {\n    id 'nf-boost@0.8.0'\n}\nparams.greeting = 'hi'\n";
    std::fs::write(directory.path().join("nextflow.config"), config).expect("writes the config");
    let launch = Launch {
        target: directory.path().to_owned(),
        scratch: directory.path().join("scratch"),
        ..Launch::default()
    };
    let printed = Rc::new(RefCell::new(String::new()));
    let settings = Settings {
        host: Box::new(Printed(printed.clone())),
        ..Settings::default()
    };
    let mut driver = Nextflow::new(&launch, settings).expect("a driver");
    let events = Rc::new(RefCell::new(Vec::new()));
    driver.observe(Box::new(Recorder(events.clone())));
    let execution = Engine::new(&mut Resumed).run("main.nf", &mut driver);
    assert!(execution.error.is_some(), "SECOND fails the run");
    assert!(
        printed.borrow().starts_with(
            "WARN: The plugin `nf-boost` is planned in uwe but not implemented yet; its \
             features are skipped\n"
        ),
        "{}",
        printed.borrow()
    );
    let events = events.borrow();
    let summaries: Vec<String> = events.iter().map(summary).collect();
    assert_eq!(
        summaries,
        [
            "run started",
            "submitted FIRST",
            "cached FIRST",
            "submitted SECOND",
            "published /work/out.txt as out.txt",
            "started SECOND",
            "failed SECOND",
            "run ended",
        ]
    );
    let Event::RunStarted(start) = &events[0] else {
        panic!("the run starts first");
    };
    assert_eq!(start.run_name, "uwe");
    assert_eq!(start.params["greeting"], "hi");
    assert!(start.script.ends_with("main.nf"), "{}", start.script);
    let Event::TaskSubmitted(submitted) = &events[1] else {
        panic!("FIRST is submitted");
    };
    assert_eq!(submitted["status"], "SUBMITTED");
    assert_eq!(submitted["task_id"], 1);
    assert!(submitted["exit"].is_null());
    let Event::TaskCached(cached) = &events[2] else {
        panic!("FIRST is cached");
    };
    assert_eq!(cached["status"], "CACHED");
    assert_eq!(cached["process"], "FIRST");
    let Event::TaskFailed(failed) = &events[6] else {
        panic!("SECOND fails");
    };
    assert_eq!(
        (&failed["status"], &failed["exit"]),
        (&"FAILED".into(), &1.into())
    );
    assert_eq!(failed["attempt"], 1);
    let Event::RunEnded(end) = &events[7] else {
        panic!("the run ends last");
    };
    assert!(!end.success);
    assert!(end.error.is_some());
    let counts = Counts {
        succeeded: 0,
        cached: 1,
        failed: 1,
    };
    assert_eq!(end.counts, counts);
}
