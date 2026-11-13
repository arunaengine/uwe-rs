//! Runs the WDL demos with the simulator and checks their tasks, inputs and outputs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use serde_json::{Value as Json, json};
use wdl_runtime::{Options, WdlDriver};
use workflow_graph::{Driver, Launch, Order, RunOptions, Simulator, Task, execute_with};

fn demo(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../demo/wdl")
        .join(name)
}

/// What a simulated run gave: its tasks in the order they were asked for, and its outputs.
struct Simulated {
    tasks: Vec<Task>,
    /// The output patterns of each task.
    patterns: Vec<Vec<String>>,
    outputs: Json,
}

fn simulate(launch: Launch, order: Order) -> Result<Simulated, String> {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let launch = Launch {
        simulated: true,
        scratch: folder.path().join("scratch"),
        ..launch
    };
    let mut driver = WdlDriver::new(&launch, Options::default()).map_err(|error| error.message)?;
    let mut simulator = Simulator::new(folder.path());
    let options = RunOptions {
        max_pending: 8,
        order,
    };
    let execution = execute_with("wdl", &mut driver, &mut simulator, &options);
    if let Some(error) = execution.error {
        return Err(error.to_string());
    }
    let mut tasks = Vec::new();
    let mut patterns = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(folder.path().join("tes"))
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    entries.sort_by_key(|path| {
        path.file_stem()
            .and_then(|stem| stem.to_str()?.parse::<u64>().ok())
    });
    for path in entries {
        let document: Json =
            serde_json::from_str(&std::fs::read_to_string(path).expect("a TES file"))
                .expect("JSON");
        let outputs = document["outputs"].as_array().cloned().unwrap_or_default();
        let paths = outputs.iter().filter_map(|output| output["path"].as_str());
        patterns.push(
            paths
                .map(|path| path.rsplit("/work/").next().unwrap_or(path).to_owned())
                .collect(),
        );
        tasks.push(Task {
            name: document["name"].as_str().unwrap_or_default().to_owned(),
            ..Task::default()
        });
    }
    let outputs = driver.results().unwrap_or(Json::Null);
    Ok(Simulated {
        tasks,
        patterns,
        outputs,
    })
}

fn launch(name: &str) -> Launch {
    Launch {
        target: demo(name),
        ..Launch::default()
    }
}

fn names(run: &Simulated) -> Vec<String> {
    let mut names: Vec<String> = run.tasks.iter().map(|task| task.name.clone()).collect();
    names.sort();
    names
}

#[test]
fn runs_hello() {
    let run = simulate(launch("hello.wdl"), Order::AsAsked).expect("a simulated run");
    assert_eq!(names(&run), ["say_hello"]);
    let outputs = run.outputs.as_object().expect("an output object");
    assert!(
        outputs["hello.greeting_file"]
            .as_str()
            .is_some_and(|path| path.ends_with("greeting.txt")),
        "{outputs:?}"
    );
    assert!(outputs.contains_key("hello.message"), "{outputs:?}");
}

#[test]
fn scatters_names() {
    for order in [Order::AsAsked, Order::Reversed] {
        let run = simulate(launch("scatter.wdl"), order).expect("a simulated run");
        assert_eq!(
            names(&run),
            ["collect", "greet (0)", "greet (1)", "greet (2)"]
        );
        assert!(
            run.outputs["scatter_names.all"]
                .as_str()
                .is_some_and(|path| path.ends_with("all.txt"))
        );
    }
    let mut empty = launch("scatter.wdl");
    empty.params = Vec::new();
    let folder = tempfile::tempdir().expect("a temporary directory");
    let inputs = folder.path().join("inputs.json");
    std::fs::write(&inputs, json!({"scatter_names.names": []}).to_string()).expect("writes inputs");
    empty.inputs = Some(inputs);
    let run = simulate(empty, Order::AsAsked).expect("a simulated run");
    assert_eq!(names(&run), ["collect"]);
}

#[test]
fn branches_on_conditions() {
    let run = simulate(launch("conditional.wdl"), Order::AsAsked).expect("a simulated run");
    assert_eq!(names(&run), ["shout"]);
    let mut quiet = launch("conditional.wdl");
    quiet.params = vec![
        ("loud".into(), "false".into()),
        ("text".into(), "psst".into()),
    ];
    let run = simulate(quiet, Order::AsAsked).expect("a simulated run");
    assert!(run.tasks.is_empty());
    assert_eq!(run.outputs, json!({"conditional.result": "psst"}));
}

#[test]
fn describes_schemas() {
    let mut driver = WdlDriver::new(&launch("scatter.wdl"), Options::default()).expect("a driver");
    let inputs = driver.inputs().expect("inputs").expect("a schema");
    assert_eq!(
        inputs.schema["properties"]["names"],
        json!({"type": "array", "items": {"type": "string"}})
    );
    assert_eq!(inputs.schema.get("required"), None);
    let outputs = driver.outputs().expect("outputs").expect("a schema");
    assert_eq!(
        outputs["properties"]["all"],
        json!({"type": "string", "format": "file-path"})
    );
    assert_eq!(outputs["required"], json!(["all", "count"]));
}

#[test]
fn rejects_unknown_inputs() {
    let mut wrong = launch("hello.wdl");
    wrong.params = vec![("greting".into(), "Hi".into())];
    let error = simulate(wrong, Order::AsAsked).err().expect("an error");
    assert!(error.contains("no input `greting`"), "{error}");
}

#[test]
fn reports_analysis_errors() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let path = folder.path().join("broken.wdl");
    std::fs::write(&path, "version 1.2\nworkflow w {\n  Int x = \"text\"\n}\n").expect("writes");
    let error = WdlDriver::new(
        &Launch {
            target: path,
            ..Launch::default()
        },
        Options::default(),
    )
    .err()
    .expect("an error");
    assert!(error.message.contains("broken.wdl:3"), "{}", error.message);
}

#[test]
fn plans_task_outputs() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let path = folder.path().join("pick.wdl");
    let document = "version 1.2\ntask pick {\n  command <<<\n    touch a.csv b.csv\n  >>>\n  output {\n    Array[File] csvs = glob(\"*.csv\")\n    File first = csvs[0]\n    Boolean same = read_string(\"a.csv\") == read_string(\"b.csv\")\n  }\n}\n";
    std::fs::write(&path, document).expect("writes");
    let run = simulate(
        Launch {
            target: path,
            ..Launch::default()
        },
        Order::AsAsked,
    )
    .expect("a simulated run");
    assert_eq!(run.outputs["pick.same"], json!(true));
    assert!(
        run.outputs["pick.first"]
            .as_str()
            .is_some_and(|path| path.ends_with("a.csv")),
        "{}",
        run.outputs
    );
    assert_eq!(run.patterns, [["*.csv", "a.csv", "b.csv"]]);
}

#[test]
fn exports_typed_outputs() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let launch = Launch {
        scratch: folder.path().join("scratch"),
        simulated: true,
        ..launch("scatter.wdl")
    };
    let mut driver = WdlDriver::new(&launch, Options::default()).expect("a driver");
    assert_eq!(driver.exported(), None, "no outputs before the run");
    let mut simulator = Simulator::new(folder.path());
    let options = RunOptions {
        max_pending: 8,
        order: Order::AsAsked,
    };
    let execution = execute_with("wdl", &mut driver, &mut simulator, &options);
    assert!(execution.error.is_none(), "{:?}", execution.error);
    let exported = driver.exported().expect("the outputs");
    let all = &exported["all"];
    assert_eq!(all["class"], "File", "{exported}");
    assert_eq!(all["basename"], "all.txt");
    let results = driver.results().expect("the results");
    assert_eq!(all["location"], results["scatter_names.all"]);
    assert!(exported["count"].is_i64(), "{exported}");
}

/// Writes its words with `write_lines` for a task that reads them.
const WRITTEN: &str = "version 1.1\n\ntask show {\n  input { File lines }\n  \
    command <<< cat ~{lines} >>>\n  output { String said = read_string(stdout()) }\n}\n\n\
    workflow written {\n  input { Array[String] words }\n  \
    call show { input: lines = write_lines(words) }\n}\n";

#[test]
fn keeps_scratch_apart() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(folder.path().join("written.wdl"), WRITTEN).expect("writes the document");
    let mut drivers = Vec::new();
    let mut tasks = Vec::new();
    for word in ["first", "second"] {
        let inputs = folder.path().join(format!("{word}.json"));
        let given = json!({"written.words": [word]}).to_string();
        std::fs::write(&inputs, given).expect("writes the inputs");
        let launch = Launch {
            target: folder.path().join("written.wdl"),
            inputs: Some(inputs),
            scratch: folder.path().join("scratch"),
            ..Launch::default()
        };
        let mut driver = WdlDriver::new(&launch, Options::default()).expect("a driver");
        tasks.push(driver.start().expect("the first tasks"));
        drivers.push(driver);
    }
    for (word, started) in ["first", "second"].into_iter().zip(tasks) {
        let [task] = started.as_slice() else {
            panic!("one task: {started:?}");
        };
        let written = (task.inputs.iter())
            .find(|input| input.source.contains("written"))
            .expect("the written file");
        let text = std::fs::read_to_string(&written.source).expect("reads the written file");
        assert_eq!(text, format!("{word}\n"), "{}", written.source);
    }
}

#[test]
fn keeps_delivered_sources() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let document = "version 1.1\n\nworkflow pass {\n  input { File given }\n  \
        output { File result = given }\n}\n";
    std::fs::write(folder.path().join("pass.wdl"), document).expect("writes the document");
    let out = folder.path().join("out");
    let data = out.join("result").join("data.txt");
    std::fs::create_dir_all(data.parent().expect("a folder")).expect("makes the folder");
    std::fs::write(&data, "kept\n").expect("writes the data");
    let inputs = folder.path().join("inputs.json");
    let given = json!({"pass.given": data.to_string_lossy()}).to_string();
    std::fs::write(&inputs, given).expect("writes the inputs");
    let launch = Launch {
        target: folder.path().join("pass.wdl"),
        inputs: Some(inputs),
        scratch: folder.path().join("scratch"),
        output_dir: Some(out),
        ..Launch::default()
    };
    let options = Options {
        print: false,
        ..Options::default()
    };
    let mut driver = WdlDriver::new(&launch, options).expect("a driver");
    assert!(driver.start().expect("starts").is_empty(), "no calls");
    driver.finish().expect("finishes");
    let deliveries = driver.deliveries();
    assert!(
        deliveries.iter().all(|item| item.location != item.target),
        "a delivery onto its own source: {deliveries:?}"
    );
    let results = driver.results().expect("the results");
    assert_eq!(results["pass.result"], json!(data.to_string_lossy()));
}
