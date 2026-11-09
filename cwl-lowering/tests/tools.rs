//! Lowers CWL tools into wir task ops, and keeps tools with unknown requirements native.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use cwl_lowering::{Lowering, lower};
use serde_json::{Value as Json, json};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn lowered(name: &str) -> Lowering {
    let path = fixtures().join(name);
    let document = cwl_parser::load_file(&path, Default::default()).unwrap();
    lower(&document, &fixtures()).unwrap()
}

/// The only op of the entry graph, as JSON.
fn only_op(lowering: &Lowering) -> Json {
    let graph = &lowering.document.graphs[0];
    assert_eq!(graph.ops.len(), 1, "one op: {graph:?}");
    serde_json::to_value(&graph.ops[0]).unwrap()
}

#[test]
fn lowers_plain_tools() {
    let lowering = lowered("echo.cwl");
    assert!(lowering.full(), "{:?}", lowering.kept);
    let op = only_op(&lowering);
    let task = json!({
        "image": "debian:stable-slim",
        "command": ["echo", "-n", {"input": "message"}, {"input": "words"}],
        "captures": [{"output": "out", "from": "stdout"}],
        "stdout": "out.txt",
        "workdir": "/var/spool/cwl",
        "staging": "/var/lib/cwl/inputs",
        "staged": ["message", "unused", "words"],
        "environment": {"HOME": "/var/spool/cwl", "TMPDIR": "/tmp"},
        "results": "cwl.output.json"
    });
    assert_eq!(op["path"], "echo");
    assert_eq!(op["kind"]["task"], task);
    let inputs = json!([
        {"name": "message", "type": "string"},
        {"name": "unused", "type": {"optional": "int"}},
        {"name": "words", "type": {"optional": {"array": {"items": "string"}}}}
    ]);
    assert_eq!(op["inputs"], inputs);
    assert_eq!(op["origin"], json!({"source": "echo"}));
    let source = &lowering.document.sources[0];
    assert_eq!(
        (source.location.as_str(), source.version.as_deref()),
        ("echo.cwl", Some("v1.2"))
    );
}

#[test]
fn captures_stream_files() {
    let op = only_op(&lowered("count.cwl"));
    let task = &op["kind"]["task"];
    assert_eq!(task["image"], "docker.io/library/debian:12-slim");
    assert_eq!(task["command"], json!(["wc", "-l", {"input": "text"}]));
    assert_eq!(task["stdout"], "count.txt");
    let captures = json!([{"output": "count", "from": "stdout"}]);
    assert_eq!(
        task["captures"], captures,
        "the glob of the stdout file reads the stream"
    );
    let disk = 2048 * 1_048_576_u64;
    let resources = json!({"cpus": 2, "memory": 67_108_864, "disk": disk});
    assert_eq!(op["attributes"], json!({"resources": resources}));
}

#[test]
fn lowers_glob_expressions() {
    let lowering = lowered("split.cwl");
    assert!(lowering.full(), "{:?}", lowering.kept);
    let ops = &lowering.document.graphs[0].ops;
    let paths: Vec<&str> = ops.iter().map(|op| op.path.as_str()).collect();
    assert_eq!(paths, ["split.glob.parts", "split"]);
    let task = serde_json::to_value(&ops[1].kind).unwrap();
    let capture = json!({"output": "parts", "from": {"patterns": "glob.parts"}});
    assert_eq!(task["task"]["captures"], json!([capture]));
    let wir::Kind::Expr(expr) = &ops[0].kind else {
        panic!("an expr: {:?}", ops[0].kind);
    };
    assert_eq!(
        expr.code, "$(inputs.prefix)*",
        "a reference needs no JavaScript"
    );
}

#[test]
fn keeps_requirements_native() {
    let lowering = lowered("update.cwl");
    let reason = "the requirement InplaceUpdateRequirement";
    assert_eq!(lowering.kept[0].reason, reason);
    assert!(wir::verify(&lowering.document).is_ok());
}

#[test]
fn lowers_expression_tools() {
    let lowering = lowered("sum.cwl");
    assert!(lowering.full(), "{:?}", lowering.kept);
    let op = only_op(&lowering);
    assert_eq!(op["kind"]["call"]["graph"], "sum");
    let graph = serde_json::to_value(&lowering.document.graphs[1]).unwrap();
    assert_eq!(
        graph["inputs"][1],
        json!({"name": "b", "type": "int", "default": 2})
    );
    let ops = graph["ops"].as_array().unwrap();
    let paths: Vec<&str> = ops.iter().map(|op| op["path"].as_str().unwrap()).collect();
    assert_eq!(paths, ["both", "sum"], "an expr per output");
    let code = ops[1]["kind"]["expr"]["code"].as_str().unwrap();
    assert!(code.starts_with("${ var value = (function() {"), "{code}");
    assert!(code.ends_with(r#"})()["sum"]; return value === undefined ? null : value; }"#));
    assert_eq!(ops[1]["outputs"], json!([{"name": "out", "type": "int"}]));
    assert_eq!(
        ops[1]["inputs"][1],
        json!({"name": "b", "type": "int"}),
        "no default"
    );
}

#[test]
fn lowers_input_values() {
    let lowering = lowered("shout.cwl");
    assert!(lowering.full(), "{:?}", lowering.kept);
    let graph = serde_json::to_value(&lowering.document.graphs[0]).unwrap();
    let ops = graph["ops"].as_array().unwrap();
    let paths: Vec<&str> = ops.iter().map(|op| op["path"].as_str().unwrap()).collect();
    assert_eq!(
        paths,
        ["shout.count.valueFrom", "shout.message.valueFrom", "shout"]
    );
    let command = json!(["echo", {"input": "message.valueFrom"}, {"input": "count.valueFrom"}]);
    assert_eq!(ops[2]["kind"]["task"]["command"], command);
    let this = json!({"name": "self", "type": "int", "default": 2});
    assert_eq!(
        ops[0]["inputs"][2], this,
        "self is the input with its default"
    );
    let edge =
        json!({"from": {"port": "count"}, "to": {"op": "shout.count.valueFrom", "port": "self"}});
    assert!(graph["edges"].as_array().unwrap().contains(&edge));
}

#[test]
fn keeps_required_any() {
    let path = fixtures().join("twice.cwl");
    let document = cwl_parser::load_file(&path, Default::default()).unwrap();
    let error = lower(&document, &fixtures()).unwrap_err();
    assert!(error.contains("the Any input value"), "{error}");
}

#[test]
fn lowers_output_evaluation() {
    let lowering = lowered("names.cwl");
    assert!(lowering.full(), "{:?}", lowering.kept);
    let op = only_op(&lowering);
    let captures = &op["kind"]["task"]["captures"];
    assert_eq!(captures[0]["from"], json!({"glob": "*"}));
    let code = captures[0]["code"].as_str().unwrap();
    assert_eq!(code, "$(self.length)", "a reference needs no JavaScript");
    assert_eq!(
        captures[1],
        json!({"output": "made", "from": {"glob": "*"}})
    );
    let files = json!({"array": {"items": {"file": {}}}});
    let outputs = json!([{"name": "count", "type": "int"}, {"name": "made", "type": files}]);
    assert_eq!(op["outputs"], outputs);
}

#[test]
fn lowers_named_types() {
    let lowering = lowered("greeting.cwl");
    assert!(lowering.full(), "{:?}", lowering.kept);
    let op = only_op(&lowering);
    let mood = json!({"enum": ["happy", "sad"]});
    let person =
        json!({"record": [{"name": "mood", "type": mood}, {"name": "name", "type": "string"}]});
    let typed = |name: &str| {
        let ports = op["inputs"].as_array().unwrap();
        ports.iter().find(|port| port["name"] == name).unwrap()["type"].clone()
    };
    assert_eq!(typed("person"), person);
    assert_eq!(typed("mood"), mood);
}

#[test]
fn lowers_argument_expressions() {
    let lowering = lowered("compile.cwl");
    assert!(lowering.full(), "{:?}", lowering.kept);
    let task = &only_op(&lowering)["kind"]["task"];
    let command = task["command"].as_array().unwrap();
    assert_eq!(
        (command[0].clone(), command[1].clone()),
        (json!("javac"), json!("-d"))
    );
    assert_eq!(command[4], json!({"input": "src"}));
    assert_eq!(
        command[2]["code"], "/var/spool/cwl",
        "a known runtime field is text"
    );
    let joined = &command[3];
    assert_eq!(
        (&joined["prefix"], &joined["separate"]),
        (&json!("-J-Xss"), &json!(false))
    );
    assert_eq!(joined["code"], "2", "the cores as text");
}

#[test]
fn lowers_input_formats() {
    let lowering = lowered("formats.cwl");
    assert!(lowering.full(), "{:?}", lowering.kept);
    let file = |format: &str| json!({"file": {"format": format}});
    let formats = json!({"union": [
        file("http://edamontology.org/format_1929"),
        file("http://edamontology.org/format_1930")
    ]});
    assert_eq!(only_op(&lowering)["inputs"][0]["type"], formats);
}

#[test]
fn lowers_secondary_files() {
    let lowering = lowered("index.cwl");
    assert!(lowering.full(), "{:?}", lowering.kept);
    let op = only_op(&lowering);
    let input = json!({"file": {"secondary_files": [
        {"pattern": ".bai"},
        {"pattern": "^.txt", "required": false}
    ]}});
    assert_eq!(op["inputs"][0]["type"], input);
    let output = json!({"file": {"secondary_files": [{"pattern": ".bai", "required": false}]}});
    assert_eq!(op["outputs"][0]["type"], output, "outputs ask by default");
}

#[test]
fn lowers_working_folder() {
    let lowering = lowered("staged.cwl");
    assert!(lowering.full(), "{:?}", lowering.kept);
    let ops = &lowering.document.graphs[0].ops;
    let task = serde_json::to_value(&ops.last().unwrap().kind).unwrap();
    let files = json!([
        {"value": {"input": "genome"}},
        {"value": {"input": "entry.1"}, "path": "index.conf"}
    ]);
    assert_eq!(task["task"]["files"], files);
    let entry = ops.iter().find(|op| op.path == "staged.entry.1").unwrap();
    let wir::Kind::Expr(expr) = &entry.kind else {
        panic!("an expr: {:?}", entry.kind);
    };
    assert!(
        expr.code.contains(r#""\n""#),
        "keeps the line end: {}",
        expr.code
    );
}

#[test]
fn lowers_format_expressions() {
    let lowering = lowered("sorted.cwl");
    assert!(lowering.full(), "{:?}", lowering.kept);
    let op = only_op(&lowering);
    let capture = &op["kind"]["task"]["captures"][0];
    assert_eq!(capture["from"], "stdout");
    let format = capture["format"].as_str().unwrap();
    assert!(format.contains("inputs.text.format"), "{format}");
    assert!(
        capture.get("code").is_none(),
        "a format is no output code: {capture}"
    );
}

#[test]
fn lowers_environment_variables() {
    let lowering = lowered("environment.cwl");
    assert!(lowering.full(), "{:?}", lowering.kept);
    let ops = &lowering.document.graphs[0].ops;
    let task = serde_json::to_value(&ops.last().unwrap().kind).unwrap();
    let environment = json!({"GREETING": "hello", "HOME": "/var/spool/cwl",
        "NAME": {"input": "environment.NAME"}, "TMPDIR": "/tmp"});
    assert_eq!(task["task"]["environment"], environment);
    assert_eq!(ops[0].path, "environment.environment.NAME");
}

#[test]
fn keeps_capture_library() {
    let lowering = lowered("parsed.cwl");
    assert!(lowering.full(), "{:?}", lowering.kept);
    let task = &only_op(&lowering)["kind"]["task"];
    assert_eq!(task["captures"][0]["code"], "$(parse(self))");
    let library = json!(["function parse(files) { return parseInt(files[0].contents); }"]);
    assert_eq!(
        task["library"], library,
        "capture code sees the expressionLib"
    );
}

#[test]
fn times_with_cores() {
    let lowering = lowered("timed.cwl");
    assert!(lowering.full(), "{:?}", lowering.kept);
    let ops = &lowering.document.graphs[0].ops;
    let limits = ops
        .iter()
        .find(|op| op.path.ends_with("runtime.limits"))
        .unwrap();
    let wir::Kind::Expr(expr) = &limits.kind else {
        panic!("an expr: {:?}", limits.kind);
    };
    let filled = expr
        .code
        .find("runtime.cores = ")
        .expect("the cores in runtime");
    let read = expr.code.find("runtime.cores * 60").unwrap();
    assert!(
        filled < read,
        "the time limit sees the cores: {}",
        expr.code
    );
}

#[test]
fn reruns_without_reuse() {
    let lowering = lowered("rerun.cwl");
    assert!(lowering.full(), "{:?}", lowering.kept);
    assert_eq!(only_op(&lowering)["attributes"]["cache"], "rerun");
    // An expression sets the cache rule in the limits of the task.
    let lowering = lowered("fresh.cwl");
    assert!(lowering.full(), "{:?}", lowering.kept);
    let ops = &lowering.document.graphs[0].ops;
    let task = ops.iter().find(|op| op.path == "fresh").unwrap();
    let wir::Kind::Task(spec) = &task.kind else {
        panic!("a task: {:?}", task.kind);
    };
    assert_eq!(spec.limits.as_deref(), Some("runtime.limits"));
    assert_eq!(task.attributes.cache, None);
    let limits = ops.iter().find(|op| op.path == "fresh.runtime.limits");
    let wir::Kind::Expr(expr) = &limits.unwrap().kind else {
        panic!("an expr of the limits");
    };
    assert!(
        expr.code.contains("var reuse = (!inputs.fresh);"),
        "{}",
        expr.code
    );
    assert!(expr.code.contains("\"cache\": cache"), "{}", expr.code);
}

#[test]
fn joins_computed_items() {
    let lowering = lowered("joined.cwl");
    assert!(lowering.full(), "{:?}", lowering.kept);
    let ops = &lowering.document.graphs[0].ops;
    let task = serde_json::to_value(&ops.last().unwrap().kind).unwrap();
    let word = &task["task"]["command"][1];
    assert_eq!(
        word["separator"], ",",
        "the valueFrom keeps its separator: {word}"
    );
}

#[test]
fn keeps_ontology_formats() {
    let lowering = lowered("ontology.cwl");
    let reason = &lowering.kept.first().expect("the tool stays native").reason;
    assert!(reason.contains("ontology"), "{reason}");
}
