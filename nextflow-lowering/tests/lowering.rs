//! Small pipelines lower into call graphs, process ops with Nextflow dialect tasks, stream ops
//! with Groovy callbacks and the ops of the start of the run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_lowering::Lowering;
use nextflow_runtime::driver::Settings;
use serde_json::{Value as Json, json};
use workflow_graph::Launch;

const GREET: &str = r#"
process GREET {
    input:
    val name
    output:
    path "${name}.txt", emit: text
    script:
    "echo ${name} > ${name}.txt"
}
"#;

/// A host in the folder of the test that prints nothing.
struct Quiet(String);

impl nextflow_evaluator::interpreter::Host for Quiet {
    fn print(&mut self, _: &str) {}

    fn launch_dir(&self) -> String {
        self.0.clone()
    }
}

/// Lowers a script with the profile `test`.
fn lower(script: &str) -> Lowering {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("main.nf"), script).unwrap();
    let config = "profiles {\n    test {\n        params.greeting = 'hi'\n    }\n}\n";
    std::fs::write(directory.path().join("nextflow.config"), config).unwrap();
    let launch = Launch {
        target: directory.path().to_owned(),
        scratch: directory.path().join("scratch"),
        profiles: vec!["test".into()],
        ..Launch::default()
    };
    let settings = Settings {
        host: Box::new(Quiet(directory.path().to_string_lossy().into_owned())),
        ..Settings::default()
    };
    nextflow_lowering::lower(&launch, settings, directory.path()).unwrap()
}

fn full(script: &str) -> Json {
    let lowering = lower(script);
    assert!(lowering.full(), "nothing stays native: {:?}", lowering.kept);
    lowering.document
}

fn graph<'a>(document: &'a Json, name: &str) -> &'a Json {
    let graphs = document["graphs"].as_array().unwrap();
    let found = graphs.iter().find(|graph| graph["name"] == name);
    found.unwrap_or_else(|| panic!("graph {name}"))
}

fn op<'a>(graph: &'a Json, path: &str) -> &'a Json {
    let ops = graph["ops"].as_array().unwrap();
    let found = ops.iter().find(|op| op["path"] == path);
    found.unwrap_or_else(|| panic!("op {path} in {graph}"))
}

/// The source text of the bytes that a scope names.
fn scoped<'a>(script: &'a str, scope: &Json) -> &'a str {
    let start = scope["start"].as_u64().unwrap() as usize;
    &script[start..scope["end"].as_u64().unwrap() as usize]
}

#[test]
fn keeps_unbound_values() {
    let lowering = lower("workflow {\n    channel.value().view()\n}\n");
    assert_eq!(lowering.kept.len(), 1, "{:?}", lowering.kept);
    assert!(
        lowering.kept[0].reason.contains("binds"),
        "{:?}",
        lowering.kept
    );
    let main = graph(&lowering.document, "main");
    assert_eq!(
        op(main, "pipeline")["kind"]["native"]["profiles"],
        json!(["test"])
    );
    let document = full("workflow {\n}\n");
    let main = graph(&document, "main");
    let params = json!({"name": "params", "type": "dynamic", "default": {"greeting": "hi"}});
    assert_eq!(main["inputs"], json!([params]));
}

#[test]
fn lowers_process_calls() {
    let script = format!("{GREET}\nworkflow {{\n    GREET(channel.of('a', 'b'))\n}}\n");
    let document = full(&script);
    let source = json!({"id": "main", "language": "nextflow", "location": "main.nf"});
    assert_eq!(document["sources"], json!([source]));
    let main = graph(&document, "main");
    let values = op(main, "of.0");
    assert_eq!(values["inputs"][0]["default"], json!(["a", "b"]));
    let process = op(main, "GREET");
    assert_eq!(process["kind"], json!({"process": {"body": "task.GREET"}}));
    assert_eq!(process["inputs"][0]["flow"], "stream", "a queue channel");
    assert_eq!(process["outputs"][0]["flow"], "stream");
    let task = &op(graph(&document, "task.GREET"), "task")["kind"]["task"]["builder"];
    let task = &task["nextflow_process"];
    assert_eq!(
        (&task["entry"], &task["call"]),
        (&json!("GREET"), &json!("GREET"))
    );
    assert_eq!(task["profiles"], json!(["test"]));
    assert_eq!(
        task["inputs"],
        json!([{"port": "input.0", "native": "input/0"}])
    );
    assert_eq!(task["captures"]["singleton"], json!({"value": false}));
    assert!(scoped(&script, &task["scope"]).starts_with("process GREET {"));
}
