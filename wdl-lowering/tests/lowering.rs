//! Lowers small WDL documents, checks the ops they become, and runs each natively and lowered
//! on the simulator with the same tasks and outputs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use serde_json::{Value as Json, json};
use wdl_lowering::Lowering;
use wir::Kind;
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Driver, Launch, Simulator, execute};

/// A lowered document and both simulated runs: the number of tasks and the outputs, with
/// Files as their basenames.
struct Runs {
    lowering: Lowering,
    native: (usize, Json),
    lowered: (usize, Json),
}

fn runs(text: &str, inputs: Json) -> Runs {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let root = folder.path();
    let target = root.join("main.wdl");
    std::fs::write(&target, text).expect("writes the document");
    let lowering = wdl_lowering::lower(&target, None, root).expect("the document lowers");
    let wir = root.join("main.wir.json");
    let document = serde_json::to_string(&lowering.document).expect("serializes");
    std::fs::write(&wir, document).expect("writes the wir document");
    let job = root.join("inputs.json");
    std::fs::write(&job, inputs.to_string()).expect("writes the inputs");
    let launch = |target: PathBuf| Launch {
        target,
        inputs: Some(job.clone()),
        scratch: root.join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let options = wdl_runtime::Options {
        print: false,
        ..Default::default()
    };
    let mut native = wdl_runtime::WdlDriver::new(&launch(target), options).expect("a WDL driver");
    let count = run(&mut native, "native", root);
    let native = (count, shown(native.exported().unwrap_or_default()));
    let settings = Settings {
        print: false,
        ..Settings::default()
    };
    let mut wired = WirDriver::new(&launch(wir), settings).expect("a wir driver");
    let count = run(&mut wired, "lowered", root);
    let lowered = (count, shown(wired.outputs().unwrap_or_default()));
    Runs {
        lowering,
        native,
        lowered,
    }
}

/// The number of tasks of a simulated run.
fn run(driver: &mut dyn Driver, name: &str, root: &Path) -> usize {
    let mut simulator = Simulator::new(root.join(format!("simulator-{name}")));
    let execution = execute(name, driver, &mut simulator);
    let graph = execution.into_result().expect("the run succeeds");
    graph.nodes.len()
}

/// Outputs with each File and Directory as its basename, as the folders differ.
fn shown(value: Json) -> Json {
    match value {
        Json::Array(items) => Json::Array(items.into_iter().map(shown).collect()),
        Json::Object(object) => match object.get("class").and_then(Json::as_str) {
            Some("File" | "Directory") => object.get("basename").cloned().unwrap_or_default(),
            _ => Json::Object(object.into_iter().map(|(k, v)| (k, shown(v))).collect()),
        },
        other => other,
    }
}

/// The kinds of the ops of a graph, by path.
fn kinds(lowering: &Lowering, graph: &str) -> Vec<(String, &'static str)> {
    let graph = (lowering.document.graphs.iter()).find(|item| item.name == graph);
    let ops = graph.expect("the graph exists").ops.iter();
    ops.map(|op| {
        let kind = match &op.kind {
            Kind::Task(_) => "task",
            Kind::Call(_) => "call",
            Kind::Native(_) => "native",
            Kind::Expr(_) => "expr",
            Kind::Map(_) => "map",
            Kind::Branch(_) => "branch",
            Kind::Select(_) => "select",
            Kind::Merge(_) => "merge",
            _ => "other",
        };
        (op.path.clone(), kind)
    })
    .collect()
}

fn named(items: &[(&str, &'static str)]) -> Vec<(String, &'static str)> {
    (items.iter())
        .map(|(path, kind)| (path.to_string(), *kind))
        .collect()
}

#[test]
fn task_outputs_captured() {
    let text = r#"version 1.1
task greet {
  input {
    String name
    Int count = 2
  }
  String file = "~{name}.txt"
  command <<<
    echo hello ~{name} > ~{file}
    touch a.log b.log
    echo done > done.txt
  >>>
  output {
    File out = stdout()
    File done = "done.txt"
    File named = file
    Array[File] logs = glob("*.log")
    Int twice = count * 2
    String base = basename(named)
  }
}
"#;
    let runs = runs(text, json!({"name": "ada"}));
    assert!(runs.lowering.full(), "kept {:?}", runs.lowering.kept);
    let ops = [
        ("command", "expr"),
        ("command.outputs", "expr"),
        ("output.twice", "expr"),
        ("output.base", "expr"),
        ("greet", "task"),
    ];
    assert_eq!(kinds(&runs.lowering, "task.greet"), named(&ops));
    assert_eq!(runs.native.0, runs.lowered.0, "as many tasks");
    let (native, lowered) = (&runs.native.1, &runs.lowered.1);
    for output in ["out", "done", "twice"] {
        assert_eq!(native[output], lowered[output], "output {output}");
    }
    assert_eq!(lowered["twice"], json!(4));
    assert_eq!(native["named"], json!("ada.txt"));
    // The simulator reads the files a native command makes from its script; a task op shows
    // it no script, so it makes up the files that a glob or a copied path matches.
    assert_eq!(native["logs"], json!(["a.log", "b.log"]));
    assert_eq!(lowered["logs"], json!(["simulated.log"]));
    assert_eq!(lowered["named"], json!("simulated"));
}
