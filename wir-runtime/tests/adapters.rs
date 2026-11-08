//! Runs dialect task ops through a registered adapter: the frame, inputs and sources it starts
//! with, retries under one task id, acceptance, notes, op errors and the end of its context.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value as Json, json};
use wir_runtime::adapter::{CallId, Changes, Ending, Opening, TaskAdapter, TaskCall};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{
    Attempt, Delivery, DeliveryMode, DriverError, Executor, Launch, Marker, MarkerKind, Note,
    Outcome, OutputFile, Running, Task, TaskId, execute,
};

/// What the adapters of a run saw.
#[derive(Debug, Default)]
struct Log {
    opened: usize,
    /// The path, step and rounds of each started op, with its inputs.
    started: Vec<(String, String, Vec<u64>, Json)>,
    ended: Vec<Option<String>>,
}

/// Runs each op as one sort task, which it runs again once after a failed attempt.
struct Sorting {
    log: Arc<Mutex<Log>>,
    tasks: HashMap<TaskId, (CallId, Task)>,
    accepted: HashSet<(TaskId, u32)>,
    refuse: bool,
}

impl TaskAdapter for Sorting {
    fn start(&mut self, call: &TaskCall) -> Result<Changes, DriverError> {
        if self.refuse {
            return Err(DriverError::new("the process has no binding"));
        }
        let frame = &call.frame;
        let inputs = Json::Object(call.inputs.clone());
        let started = (frame.path.clone(), frame.step.clone(), frame.rounds.clone());
        self.log
            .lock()
            .unwrap()
            .started
            .push((started.0, started.1, started.2, inputs));
        // Ids of the adapter itself; the run gives the tasks ids of its own.
        let task = Task {
            id: TaskId(100 + self.tasks.len() as u64),
            name: format!("SORT ({})", frame.rounds[0]),
            step: frame.step.clone(),
            command: vec!["sort".into()],
            outputs: vec!["sorted.txt".into()],
            ..Task::default()
        };
        self.tasks.insert(task.id, (call.call, task.clone()));
        Ok(Changes {
            tasks: vec![(call.call, task)],
            ended: Vec::new(),
            ready: Vec::new(),
        })
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Changes, DriverError> {
        let (call, task) = self.tasks[&outcome.id].clone();
        if outcome.exit_code != 0 {
            return Ok(Changes {
                tasks: vec![(call, task)],
                ended: Vec::new(),
                ready: Vec::new(),
            });
        }
        self.accepted.insert((outcome.id, outcome.attempt));
        let file = json!({"class": "File", "location": outcome.files[0].location});
        let values = Map::from_iter([("sorted".to_owned(), file)]);
        Ok(Changes {
            tasks: Vec::new(),
            ended: vec![(
                call,
                Ending::Succeeded {
                    values,
                    empty: Vec::new(),
                },
            )],
            ready: Vec::new(),
        })
    }

    fn take_acceptance(&mut self, task: TaskId, attempt: u32) -> bool {
        self.accepted.remove(&(task, attempt))
    }

    fn end(&mut self, failure: Option<&str>) -> Result<Vec<Delivery>, DriverError> {
        self.log
            .lock()
            .unwrap()
            .ended
            .push(failure.map(str::to_owned));
        Ok(Vec::new())
    }
}

/// A document whose map `each` sorts each of `texts` with the dialect task `sort`.
fn document(root: &Path, texts: usize) -> Launch {
    let files = json!({"array": {"items": {"file": {}}}});
    let edge = |from: (Option<&str>, &str), to: (Option<&str>, &str)| json!({"from": {"op": from.0, "port": from.1}, "to": {"op": to.0, "port": to.1}});
    let builder = json!({"nextflow_process": {"source": "nf", "entry": "main", "call": "SORT",
        "inputs": [{"port": "text", "native": "input/0"}], "outputs": [{"port": "sorted", "native": "output/0"}]}});
    let document = json!({
        "wir_version": wir::VERSION,
        "sources": [{"id": "nf", "language": "nextflow", "location": "main.nf", "digest": "sha256:ab"}],
        "graphs": [{
            "name": "main",
            "inputs": [{"name": "texts", "type": files}],
            "outputs": [{"name": "outs", "type": files}],
            "ops": [{
                "path": "each",
                "kind": {"map": {"body": "round", "over": ["text"]}},
                "inputs": [{"name": "text", "type": files}],
                "outputs": [{"name": "sorted", "type": files}]
            }],
            "edges": [
                edge((None, "texts"), (Some("each"), "text")),
                edge((Some("each"), "sorted"), (None, "outs"))
            ]
        }, {
            "name": "round",
            "inputs": [{"name": "text", "type": {"file": {}}}],
            "outputs": [{"name": "sorted", "type": {"file": {}}}],
            "ops": [{
                "path": "sort",
                "kind": {"task": {"builder": builder}},
                "inputs": [{"name": "text", "type": {"file": {}}}],
                "outputs": [{"name": "sorted", "type": {"file": {}}}]
            }],
            "edges": [
                edge((None, "text"), (Some("sort"), "text")),
                edge((Some("sort"), "sorted"), (None, "sorted"))
            ]
        }],
        "entry": "main"
    });
    std::fs::write(root.join("sort.wir.json"), document.to_string()).unwrap();
    let texts: Vec<Json> = (0..texts)
        .map(|index| {
            std::fs::write(root.join(format!("{index}.txt")), "b\na\n").unwrap();
            json!({"class": "File", "location": format!("{index}.txt")})
        })
        .collect();
    std::fs::write(
        root.join("inputs.json"),
        json!({"texts": texts}).to_string(),
    )
    .unwrap();
    Launch {
        target: root.join("sort.wir.json"),
        inputs: Some(root.join("inputs.json")),
        scratch: root.join("scratch"),
        output_dir: Some(root.join("out")),
        ..Launch::default()
    }
}

/// A driver whose `nextflow_process` tasks run through [`Sorting`] adapters.
fn driver(launch: &Launch, log: &Arc<Mutex<Log>>, refuse: bool) -> WirDriver {
    let mut settings = Settings {
        print: false,
        ..Settings::default()
    };
    let shared = Arc::clone(log);
    settings
        .adapters
        .register("nextflow_process", move |opening| {
            assert!(opening.source.ends_with("main.nf"));
            shared.lock().unwrap().opened += 1;
            Ok(Box::new(Sorting {
                log: Arc::clone(&shared),
                tasks: HashMap::new(),
                accepted: HashSet::new(),
                refuse,
            }))
        });
    WirDriver::new(launch, settings).unwrap()
}

/// Writes the outputs of each attempt; the first attempt of round 0 fails.
struct Sorter {
    root: PathBuf,
    attempts: Vec<(TaskId, String, u32)>,
}

impl Executor for Sorter {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = &attempt.task;
        self.attempts
            .push((task.id, task.name.clone(), attempt.number));
        let work = self.root.join(format!("{}-{}", task.id, attempt.number));
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("sorted.txt"), "a\nb\n").unwrap();
        let failing = task.name.ends_with("(0)") && attempt.number == 1;
        let outcome = Outcome {
            id: task.id,
            attempt: attempt.number,
            exit_code: i32::from(failing),
            files: vec![OutputFile {
                path: "sorted.txt".into(),
                location: work.join("sorted.txt").to_string_lossy().into_owned(),
                ..OutputFile::default()
            }],
            ..Outcome::default()
        };
        Running::ended(Ok(outcome))
    }
}

#[test]
fn runs_dialect_tasks() {
    let folder = tempfile::tempdir().unwrap();
    let log = Arc::new(Mutex::new(Log::default()));
    let mut driver = driver(&document(folder.path(), 2), &log, false);
    let mut executor = Sorter {
        root: folder.path().join("work"),
        attempts: Vec::new(),
    };
    execute("adapters", &mut driver, &mut executor)
        .into_result()
        .unwrap();
    let log = log.lock().unwrap();
    assert_eq!(log.opened, 1);
    let frames: Vec<_> = (log.started.iter())
        .map(|(path, step, rounds, _)| (path.as_str(), step.as_str(), rounds.clone()))
        .collect();
    let expected = [
        ("each/0/sort", "each/sort", vec![0]),
        ("each/1/sort", "each/sort", vec![1]),
    ];
    assert_eq!(frames, expected);
    let text = &log.started[1].3["text"];
    assert!(
        text["location"].as_str().unwrap().ends_with("1.txt"),
        "{text}"
    );
    assert_eq!(log.ended, [None]);
    // The retry keeps the run id of its task, and no task keeps an id of the adapter.
    let mut attempts = executor.attempts.clone();
    attempts.sort();
    let names: Vec<(&str, u32)> = (attempts.iter())
        .map(|(_, name, number)| (name.as_str(), *number))
        .collect();
    let expected = [("each/0/sort: SORT (0)", 1), ("each/0/sort: SORT (0)", 2)];
    assert_eq!(names[..2], expected);
    assert_eq!(names[2], ("each/1/sort: SORT (1)", 1));
    assert_eq!(attempts[0].0, attempts[1].0);
    assert!(attempts.iter().all(|(id, _, _)| id.0 < 100));
    let outs = driver.outputs().unwrap()["outs"].clone();
    assert_eq!(outs.as_array().map(Vec::len), Some(2), "{outs}");
}

#[test]
fn errors_refused_ops() {
    let folder = tempfile::tempdir().unwrap();
    let log = Arc::new(Mutex::new(Log::default()));
    let mut driver = driver(&document(folder.path(), 1), &log, true);
    let mut executor = Sorter {
        root: folder.path().join("work"),
        attempts: Vec::new(),
    };
    let error = execute("adapters", &mut driver, &mut executor)
        .into_result()
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("op each/0/sort: the process has no binding"),
        "{error}"
    );
    assert!(executor.attempts.is_empty());
    let ended = &log.lock().unwrap().ended;
    let failure = ended[0].as_deref().unwrap_or_default();
    assert!(failure.contains("the process has no binding"), "{ended:?}");
}

/// Runs each op as one task; the op `d` reports its daemon ready as it starts.
#[derive(Default)]
struct Daemons {
    started: u64,
}

impl TaskAdapter for Daemons {
    fn start(&mut self, call: &TaskCall) -> Result<Changes, DriverError> {
        self.started += 1;
        let task = Task {
            id: TaskId(self.started),
            name: call.frame.path.clone(),
            ..Task::default()
        };
        let ready = match call.frame.path.as_str() {
            "d" => vec![call.call],
            _ => Vec::new(),
        };
        Ok(Changes {
            tasks: vec![(call.call, task)],
            ended: Vec::new(),
            ready,
        })
    }

    fn complete(&mut self, _: Outcome) -> Result<Changes, DriverError> {
        Ok(Changes::default())
    }
}

#[test]
fn daemon_readiness() {
    let folder = tempfile::tempdir().unwrap();
    let task = |call: &str| json!({"task": {"builder": {"argo_task": {"source": "wf", "entry": "main", "call": call}}}});
    let after = json!({"result": {"op": "d", "on": ["daemoned"]}});
    let document = json!({
        "wir_version": wir::VERSION,
        "sources": [{"id": "wf", "language": "argo", "location": "wf.yaml", "digest": "sha256:ab"}],
        "graphs": [{"name": "main", "ops": [
            {"path": "d", "kind": task(".d")},
            {"path": "c", "kind": task(".c"), "depends": after}
        ]}],
        "entry": "main"
    });
    std::fs::write(folder.path().join("run.wir.json"), document.to_string()).unwrap();
    let launch = Launch {
        target: folder.path().join("run.wir.json"),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let mut settings = Settings {
        print: false,
        ..Settings::default()
    };
    (settings.adapters).register("argo_task", |_| Ok(Box::new(Daemons::default())));
    let mut driver = WirDriver::new(&launch, settings).unwrap();
    let tasks = workflow_graph::Driver::start(&mut driver).unwrap();
    let names: Vec<&str> = tasks.iter().map(|task| task.name.as_str()).collect();
    assert_eq!(
        names,
        ["d: d", "c: c"],
        "c starts once the daemon d is ready"
    );
}

/// Ends each op at once with a value for its output `a` and its output `b` empty, as a
/// success, or as a skip when `skipped` is set.
struct Partial {
    skipped: bool,
}

impl TaskAdapter for Partial {
    fn start(&mut self, call: &TaskCall) -> Result<Changes, DriverError> {
        let values = Map::from_iter([("a".to_owned(), json!("made"))]);
        let ending = match self.skipped {
            true => Ending::Skipped(values),
            false => Ending::Succeeded {
                values,
                empty: vec!["b".into()],
            },
        };
        Ok(Changes {
            ended: vec![(call.call, ending)],
            ..Changes::default()
        })
    }

    fn complete(&mut self, _: Outcome) -> Result<Changes, DriverError> {
        Ok(Changes::default())
    }
}

/// The results of a run of the op `p`, which [`Partial`] ends, skipped when `skipped` is set.
fn partial(skipped: bool) -> Json {
    let folder = tempfile::tempdir().unwrap();
    let maps = json!([{"port": "a", "native": "output/0"}, {"port": "b", "native": "output/1"}]);
    let builder = json!({"nextflow_process": {"source": "nf", "entry": "main", "call": "P", "outputs": maps}});
    let ports = json!([{"name": "a", "type": "string"}, {"name": "b", "type": "string"}]);
    let edge = |port: &str| json!({"from": {"op": "p", "port": port}, "to": {"port": port}});
    let document = json!({
        "wir_version": wir::VERSION,
        "sources": [{"id": "nf", "language": "nextflow", "location": "main.nf", "digest": "sha256:ab"}],
        "graphs": [{"name": "main", "outputs": ports,
            "ops": [{"path": "p", "kind": {"task": {"builder": builder}}, "outputs": ports}],
            "edges": [edge("a"), edge("b")]}],
        "entry": "main"
    });
    std::fs::write(folder.path().join("run.wir.json"), document.to_string()).unwrap();
    let launch = Launch {
        target: folder.path().join("run.wir.json"),
        scratch: folder.path().join("scratch"),
        output_dir: Some(folder.path().join("out")),
        ..Launch::default()
    };
    let mut settings = Settings {
        print: false,
        ..Settings::default()
    };
    let adapter = move |_: &Opening| -> Result<Box<dyn TaskAdapter>, DriverError> {
        Ok(Box::new(Partial { skipped }))
    };
    (settings.adapters).register("nextflow_process", adapter);
    let mut driver = WirDriver::new(&launch, settings).unwrap();
    assert!(
        workflow_graph::Driver::start(&mut driver)
            .unwrap()
            .is_empty()
    );
    workflow_graph::Driver::finish(&mut driver).unwrap();
    workflow_graph::Driver::results(&driver).unwrap()
}

#[test]
fn empty_outputs() {
    assert_eq!(
        partial(false),
        json!({"a": "made"}),
        "the empty output b is no null"
    );
    let skipped = partial(true);
    assert_eq!(
        skipped,
        json!({"a": "made"}),
        "a skip gives its values; the rest are empty"
    );
}

/// Runs each op as the task 100 of the adapter, which it marks as a checkpoint.
#[derive(Default)]
struct Marking {
    marked: bool,
}

impl TaskAdapter for Marking {
    fn start(&mut self, call: &TaskCall) -> Result<Changes, DriverError> {
        let task = Task {
            id: TaskId(100),
            ..Task::default()
        };
        Ok(Changes {
            tasks: vec![(call.call, task)],
            ..Changes::default()
        })
    }

    fn complete(&mut self, _: Outcome) -> Result<Changes, DriverError> {
        Ok(Changes::default())
    }

    fn notes(&mut self) -> Vec<Note> {
        if std::mem::replace(&mut self.marked, true) {
            return Vec::new();
        }
        let marker = Marker {
            kind: MarkerKind::Checkpoint,
            reason: "a checkpoint".into(),
        };
        vec![Note::Mark(TaskId(100), marker)]
    }

    fn end(&mut self, _: Option<&str>) -> Result<Vec<Delivery>, DriverError> {
        let report = Delivery {
            location: "report.html".into(),
            target: "out/report.html".into(),
            mode: DeliveryMode::Copy,
            overwrite: true,
        };
        Ok(vec![report])
    }
}

#[test]
fn adapter_notes() {
    let folder = tempfile::tempdir().unwrap();
    let launch = document(folder.path(), 1);
    let mut settings = Settings {
        print: false,
        ..Settings::default()
    };
    let target = &launch.target;
    let mut written: Json =
        serde_json::from_str(&std::fs::read_to_string(target).unwrap()).unwrap();
    written["hooks"] =
        json!([{"observe": {"source": "nf", "processes": [{"id": 1, "name": "SORT"}]}}]);
    std::fs::write(target, written.to_string()).unwrap();
    settings.adapters.register("nextflow_process", |opening| {
        let observed = &opening.observers;
        assert_eq!(
            observed.len(),
            1,
            "the opening has the observers of its source"
        );
        assert_eq!(observed[0].processes[0].name, "SORT");
        let sources: Vec<&str> = opening
            .sources
            .iter()
            .map(|source| source.id.as_str())
            .collect();
        assert_eq!(
            sources,
            ["nf"],
            "the opening has the sources of the document"
        );
        Ok(Box::new(Marking::default()))
    });
    let mut driver = WirDriver::new(&launch, settings).unwrap();
    let tasks = workflow_graph::Driver::start(&mut driver).unwrap();
    let notes = workflow_graph::Driver::notes(&mut driver);
    let [Note::Mark(task, marker)] = &notes[..] else {
        panic!("one mark: {notes:?}");
    };
    assert_eq!(
        (*task, marker.kind),
        (tasks[0].id, MarkerKind::Checkpoint),
        "under the run id"
    );
    // The run fails, as the task never ended; the report of the end is delivered anyway.
    assert!(workflow_graph::Driver::finish(&mut driver).is_err());
    let delivered = workflow_graph::Driver::deliveries(&mut driver);
    let targets: Vec<&str> = delivered.iter().map(|item| item.target.as_str()).collect();
    assert_eq!(targets, ["out/report.html"]);
}

/// Ends each op at once with the File `work/report.html` as its output `a`, and delivers
/// another `report.html` to the output folder at its end.
struct Reporting {
    root: PathBuf,
}

impl TaskAdapter for Reporting {
    fn start(&mut self, call: &TaskCall) -> Result<Changes, DriverError> {
        let made = self.root.join("work/report.html");
        let file = json!({"class": "File", "location": made, "basename": "report.html"});
        let ending = Ending::Succeeded {
            values: Map::from_iter([("a".to_owned(), file)]),
            empty: Vec::new(),
        };
        Ok(Changes {
            ended: vec![(call.call, ending)],
            ..Changes::default()
        })
    }

    fn complete(&mut self, _: Outcome) -> Result<Changes, DriverError> {
        Ok(Changes::default())
    }

    fn end(&mut self, _: Option<&str>) -> Result<Vec<Delivery>, DriverError> {
        let report = Delivery {
            location: self
                .root
                .join("reports/report.html")
                .to_string_lossy()
                .into_owned(),
            target: self
                .root
                .join("out/report.html")
                .to_string_lossy()
                .into_owned(),
            mode: DeliveryMode::Copy,
            overwrite: true,
        };
        Ok(vec![report])
    }
}

#[test]
fn report_places() {
    let folder = tempfile::tempdir().unwrap();
    let maps = json!([{"port": "a", "native": "output/0"}]);
    let builder = json!({"nextflow_process": {"source": "nf", "entry": "main", "call": "P", "outputs": maps}});
    let ports = json!([{"name": "a", "type": {"file": {}}}]);
    let document = json!({
        "wir_version": wir::VERSION,
        "sources": [{"id": "nf", "language": "nextflow", "location": "main.nf", "digest": "sha256:ab"}],
        "graphs": [{"name": "main", "outputs": ports,
            "ops": [{"path": "p", "kind": {"task": {"builder": builder}}, "outputs": ports}],
            "edges": [{"from": {"op": "p", "port": "a"}, "to": {"port": "a"}}]}],
        "entry": "main"
    });
    std::fs::write(folder.path().join("run.wir.json"), document.to_string()).unwrap();
    let launch = Launch {
        target: folder.path().join("run.wir.json"),
        scratch: folder.path().join("scratch"),
        output_dir: Some(folder.path().join("out")),
        ..Launch::default()
    };
    let mut settings = Settings {
        print: false,
        ..Settings::default()
    };
    let root = folder.path().to_owned();
    settings.adapters.register("nextflow_process", move |_| {
        Ok(Box::new(Reporting { root: root.clone() }))
    });
    let mut driver = WirDriver::new(&launch, settings).unwrap();
    workflow_graph::Driver::start(&mut driver).unwrap();
    workflow_graph::Driver::finish(&mut driver).unwrap();
    let delivered = workflow_graph::Driver::deliveries(&mut driver);
    let mut targets: Vec<String> = delivered.iter().map(|item| item.target.clone()).collect();
    targets.sort();
    targets.dedup();
    assert_eq!(
        targets.len(),
        2,
        "the graph output takes another place: {targets:?}"
    );
}
