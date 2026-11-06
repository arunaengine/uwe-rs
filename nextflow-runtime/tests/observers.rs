//! Run observers hear the events of a real run in order: its start, each task, the files it
//! publishes and its end, with the trace records of the tasks.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use nextflow_evaluator::interpreter::Host;
use nextflow_plugins::observer::{Counts, Event, File, Observer, Task};
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

/// Keeps every event it hears, and opens the gate of SECOND once it hears that it started.
struct Recorder {
    events: Rc<RefCell<Vec<Event>>>,
    gate: Arc<Mutex<Gate>>,
}

impl Observer for Recorder {
    fn notify(&mut self, event: &Event) {
        if let Event::TaskStarted(task) = event
            && task.record["name"] == "SECOND"
        {
            open(&self.gate, "started");
        }
        self.events.borrow_mut().push(event.clone());
    }
}

/// Holds the attempt of SECOND open, with what opened it.
#[derive(Default)]
struct Gate {
    opener: Option<&'static str>,
    waker: Option<Waker>,
}

fn open(gate: &Mutex<Gate>, opener: &'static str) {
    let mut gate = gate.lock().expect("not poisoned");
    if gate.opener.is_none() {
        gate.opener = Some(opener);
        if let Some(waker) = gate.waker.take() {
            waker.wake();
        }
    }
}

/// Ends when the gate opens.
struct Opened(Arc<Mutex<Gate>>);

impl Future for Opened {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        let mut gate = self.0.lock().expect("not poisoned");
        if gate.opener.is_some() {
            return Poll::Ready(());
        }
        gate.waker = Some(context.waker().clone());
        Poll::Pending
    }
}

/// Keeps what the run prints.
struct Printed(Rc<RefCell<String>>);

impl Host for Printed {
    fn print(&mut self, text: &str) {
        self.0.borrow_mut().push_str(text);
    }
}

/// Answers FIRST from the cache, as on resume, and fails SECOND once its gate opens.
struct Resumed(Arc<Mutex<Gate>>);

impl Executor for Resumed {
    fn run(&mut self, attempt: Attempt) -> Running {
        let id = attempt.task.id;
        if attempt.task.step.ends_with("FIRST") {
            return Running::ended(Ok(Outcome {
                id,
                cached: true,
                files: vec![OutputFile {
                    path: "out.txt".into(),
                    location: "/work/out.txt".into(),
                    ..OutputFile::default()
                }],
                ..Outcome::default()
            }));
        }
        let gate = Opened(self.0.clone());
        Running::new(async move {
            gate.await;
            Ok(Outcome {
                id,
                exit_code: 1,
                ..Outcome::default()
            })
        })
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
    let name = |task: &Task| task.record["name"].as_str().unwrap_or_default().to_owned();
    match event {
        Event::RunStarted(_) => "run started".into(),
        Event::TaskSubmitted(task) => format!("submitted {}", name(task)),
        Event::TaskStarted(task) => format!("started {}", name(task)),
        Event::TaskCompleted(task) => format!("completed {}", name(task)),
        Event::TaskCached(task) => format!("cached {}", name(task)),
        Event::TaskFailed(task) => format!("failed {}", name(task)),
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
    let config = "plugins {\n    id 'nf-prov@1.4.0'\n}\nparams.greeting = 'hi'\n";
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
    let gate = Arc::new(Mutex::new(Gate::default()));
    driver.observe(Box::new(Recorder {
        events: events.clone(),
        gate: gate.clone(),
    }));
    // Without the started event the attempt would wait forever; this ends it, and fails the test.
    let fallback = gate.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(60));
        open(&fallback, "fallback");
    });
    let execution = Engine::new(&mut Resumed(gate.clone())).run("main.nf", &mut driver);
    assert!(execution.error.is_some(), "SECOND fails the run");
    let opener = gate.lock().expect("not poisoned").opener;
    assert_eq!(
        opener,
        Some("started"),
        "SECOND started while its attempt ran"
    );
    assert!(
        printed.borrow().starts_with(
            "WARN: The plugin `nf-prov` is planned in uwe but not implemented yet; its \
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
    let record = &submitted.record;
    assert_eq!(record["status"], "SUBMITTED");
    assert_eq!(record["task_id"], 1);
    assert!(record["exit"].is_null());
    assert_eq!(submitted.hash.len(), 64, "{}", submitted.hash);
    let short = format!("{}/{}", &submitted.hash[..2], &submitted.hash[2..8]);
    assert_eq!(record["hash"], short.as_str());
    assert_eq!(submitted.executor, "local");
    assert!(
        record["script"]
            .as_str()
            .is_some_and(|script| script.contains("echo a > out.txt"))
    );
    assert!(record["tag"].is_null());
    assert!(submitted.inputs.is_empty() && submitted.outputs.is_empty());
    let Event::TaskCached(cached) = &events[2] else {
        panic!("FIRST is cached");
    };
    assert_eq!(cached.record["status"], "CACHED");
    assert_eq!(cached.record["process"], "FIRST");
    assert_eq!(cached.hash, submitted.hash);
    let made = File {
        name: "out.txt".into(),
        path: "/work/out.txt".into(),
        digest: None,
    };
    assert_eq!(cached.outputs, std::slice::from_ref(&made));
    let Event::TaskStarted(started) = &events[5] else {
        panic!("SECOND starts");
    };
    assert_eq!(started.record["status"], "RUNNING");
    assert!(started.record["start"].is_i64() && started.record["complete"].is_null());
    let Event::TaskFailed(failed) = &events[6] else {
        panic!("SECOND fails");
    };
    let record = &failed.record;
    assert_eq!(
        (&record["status"], &record["exit"]),
        (&"FAILED".into(), &1.into())
    );
    assert_eq!(record["attempt"], 1);
    assert_eq!(failed.inputs, [made]);
    assert_eq!(failed.values, [serde_json::json!("/work/out.txt")]);
    for unknown in [
        "workdir",
        "native_id",
        "cpu_model",
        "rchar",
        "rss",
        "%mem",
        "vol_ctxt",
    ] {
        assert!(record[unknown].is_null(), "{unknown} is not measured");
    }
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
