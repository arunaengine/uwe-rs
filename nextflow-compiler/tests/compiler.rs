//! Small pipelines compile into call graphs, process ops with Nextflow dialect tasks, stream ops
//! with Groovy callbacks and the ops of the start of the run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_compiler::Compilation;
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

/// Compiles a script with the profile `test`.
fn compile(script: &str) -> Compilation {
    compile_with(script, &[])
}

/// Compiles a script with the profile `test` and more files in its project folder.
fn compile_with(script: &str, files: &[(&str, &str)]) -> Compilation {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("main.nf"), script).unwrap();
    for (name, text) in files {
        std::fs::write(directory.path().join(name), text).unwrap();
    }
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
    nextflow_compiler::compile(&launch, settings, directory.path()).unwrap()
}

/// The document of a compilation as JSON.
fn json(compiled: &Compilation) -> Json {
    serde_json::to_value(&compiled.document).unwrap()
}

/// The document of a script that compiles fully, which the wir verifier accepts.
fn full(script: &str) -> Json {
    let compiled = compile(script);
    assert!(compiled.full(), "nothing stays native: {:?}", compiled.kept);
    if let Err(problems) = wir::verify(&compiled.document) {
        let shown: Vec<String> = problems.iter().map(ToString::to_string).collect();
        panic!("the document verifies: {}", shown.join("\n"));
    }
    json(&compiled)
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
    let compiled = compile("workflow {\n    channel.value().view()\n}\n");
    assert_eq!(compiled.kept.len(), 1, "{:?}", compiled.kept);
    assert!(
        compiled.kept[0].reason.contains("binds"),
        "{:?}",
        compiled.kept
    );
    let document = json(&compiled);
    let main = graph(&document, "main");
    assert_eq!(
        op(main, "pipeline")["kind"]["native"]["profiles"],
        json!(["test"])
    );
    let document = full("workflow {\n}\n");
    let main = graph(&document, "main");
    let param = json!({"name": "greeting", "type": "dynamic", "default": "hi"});
    assert_eq!(main["inputs"], json!([param]), "a port for each param");
}

#[test]
fn compiles_process_calls() {
    let script = format!("{GREET}\nworkflow {{\n    GREET(channel.of('a', 'b'))\n}}\n");
    let document = full(&script);
    let source = &document["sources"][0];
    assert_eq!(
        (&source["id"], &source["location"]),
        (&json!("main"), &json!("main.nf"))
    );
    let digest = source["digest"].as_str().unwrap_or_default();
    assert!(
        digest.starts_with("sha256:") && digest.len() == 71,
        "{digest}"
    );
    let sources = document["sources"].as_array().unwrap();
    let ids: Vec<&Json> = sources.iter().map(|source| &source["id"]).collect();
    assert_eq!(
        ids,
        [&json!("main"), &json!("nextflow.config"), &json!("run")]
    );
    let run = sources[2]["location"].as_str().unwrap();
    assert!(
        run.starts_with("data:application/json,%7B%22params%22%3A%22sha256%3A"),
        "{run}"
    );
    assert!(
        run.contains("%22config%22%3A%22sha256%3A"),
        "pins the config: {run}"
    );
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

#[test]
fn compiles_closure_callbacks() {
    let script = format!(
        "{GREET}\nworkflow SUB {{\n    take:\n    words\n    main:\n    def suffix = '!'\n    \
         GREET(words.map {{ word -> word + suffix }})\n    emit:\n    text = GREET.out.text\n}}\n\n\
         workflow {{\n    SUB(channel.of('a'))\n    SUB.out.text.collect().view()\n}}\n"
    );
    let document = full(&script);
    let main = graph(&document, "main");
    assert_eq!(
        op(main, "SUB")["kind"],
        json!({"call": {"graph": "workflow.SUB"}})
    );
    let sub = graph(&document, "workflow.SUB");
    assert_eq!(sub["inputs"].as_array().unwrap().len(), 1, "the take");
    let body = &op(sub, "map.1")["kind"]["transform"]["body"];
    assert_eq!(body["code"], "{ word -> word + suffix }");
    assert_eq!(body["captures"], json!({"0": {"value": "!"}}));
    assert_eq!(scoped(&script, &body["scope"]), body["code"]);
    let collect = op(main, "collect.3");
    assert_eq!(
        collect["outputs"][0].get("flow"),
        None,
        "collect gives a value"
    );
    let view = op(main, "view.4");
    assert_eq!(view["inputs"][0].get("flow"), None, "a view of the value");
    assert_eq!(view["effects"], json!(["log"]));
}

#[test]
fn compiles_branch_routes() {
    let script = "workflow {\n    numbers = channel.of(1, 2, 3).branch { n ->\n        \
                  small: n < 2\n        large: true\n    }\n    \
                  numbers.small.join(numbers.large).view()\n}\n";
    let document = full(script);
    let main = graph(&document, "main");
    let choose = &op(main, "branch.1")["kind"]["transform"]["body"];
    assert!(scoped(script, &choose["scope"]).starts_with("{ n ->"));
    let route = &op(main, "branch.1/choose")["kind"]["route"]["cases"];
    assert_eq!(route[0]["output"], "small");
    assert_eq!(route[1]["condition"]["code"], "{ it -> it[0] == 'large' }");
    let join = op(main, "join.2");
    assert_eq!(join["inputs"][0]["name"], "left");
    assert_eq!(join["values"], "groovy", "Groovy keys");
}

#[test]
fn starts_with_prints() {
    let script = format!("{GREET}\nworkflow {{\n    log.info 'starting'\n    GREET('a')\n}}\n");
    let document = full(&script);
    let main = graph(&document, "main");
    let print = op(main, "start.0");
    assert_eq!(print["inputs"][0]["default"], json!(["starting\n"]));
    assert_eq!(op(main, "start.0/view")["effects"], json!(["log"]));
    let process = op(main, "GREET");
    let after = json!([{"op": "start.0/view", "on": ["succeeded"]}]);
    assert_eq!(process["after"], after, "the run prints first");
    assert_eq!(process["inputs"][0]["default"], "a", "a plain value");
    assert_eq!(
        process["outputs"][0].get("flow"),
        None,
        "one task gives values"
    );
}

#[test]
fn keeps_data_reads() {
    let script = "include { samplesheetToList } from 'plugin/nf-schema'\n\nworkflow {\n    \
                  def first = samplesheetToList('sheet.csv', 'schema.json').first()\n}\n";
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("main.nf"), script).unwrap();
    let config = "plugins {\n    id 'nf-schema@2.5.1'\n}\n";
    std::fs::write(directory.path().join("nextflow.config"), config).unwrap();
    let launch = Launch {
        target: directory.path().to_owned(),
        scratch: directory.path().join("scratch"),
        ..Launch::default()
    };
    let settings = Settings {
        host: Box::new(Quiet(directory.path().to_string_lossy().into_owned())),
        ..Settings::default()
    };
    let compiled = nextflow_compiler::compile(&launch, settings, directory.path()).unwrap();
    assert!(
        compiled.kept[0].reason.contains("reads the data"),
        "{:?}",
        compiled.kept
    );
    let document = json(&compiled);
    let source = &document["sources"][0];
    assert_eq!(source["location"], ".", "the project folder");
}

#[test]
fn types_entry_ports() {
    let schema = r#"{"$defs": {"options": {"properties": {
        "sheet": {"type": "string", "format": "file-path"},
        "greeting": {"type": "string"},
        "count": {"type": "integer"}
    }}}}"#;
    let script = "params.sheet = 'sheet.csv'\nparams.count = 2\nworkflow {\n}\n";
    let compiled = compile_with(script, &[("nextflow_schema.json", schema)]);
    let document = json(&compiled);
    let inputs = &graph(&document, "main")["inputs"];
    let by = |name: &str| {
        inputs
            .as_array()
            .unwrap()
            .iter()
            .find(|port| port["name"] == name)
            .unwrap()
    };
    assert_eq!(by("sheet")["type"], json!({"file": {}}));
    let location = by("sheet")["default"]["location"].as_str().unwrap();
    assert!(
        location.ends_with("/sheet.csv") && location.starts_with('/'),
        "{location}"
    );
    assert_eq!(
        (&by("greeting")["type"], &by("count")["type"]),
        (&json!("string"), &json!("int"))
    );
    assert!(
        wir::verify(&compiled.document).is_ok(),
        "the defaults fit their types"
    );
}
