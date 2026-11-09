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
fn closes_unbound_values() {
    let document = full("workflow {\n    channel.value().view()\n}\n");
    let main = graph(&document, "main");
    let empty = op(main, "value.0");
    assert_eq!(empty["inputs"][0]["default"], json!([]), "an empty array");
    assert_eq!(op(main, "value.0/then")["kind"], json!({"first": {}}));
}

#[test]
fn keeps_typed_operators() {
    let compiled = compile("workflow {\n    channel.of(['a', 1]).groupBy().view()\n}\n");
    assert_eq!(compiled.kept.len(), 1, "{:?}", compiled.kept);
    assert!(
        compiled.kept[0]
            .reason
            .contains("`groupBy` of typed workflows"),
        "{:?}",
        compiled.kept
    );
    let document = json(&compiled);
    let main = graph(&document, "main");
    assert_eq!(
        op(main, "pipeline")["kind"]["native"]["profiles"],
        json!(["test"])
    );
    let param = json!({"name": "greeting", "type": {"optional": "string"}, "default": "hi"});
    assert_eq!(
        main["inputs"],
        json!([param]),
        "the native op keeps the params"
    );
    let edge = json!({"from": {"port": "greeting"}, "to": {"op": "pipeline", "port": "greeting"}});
    assert!(main["edges"].as_array().unwrap().contains(&edge));
    if let Err(problems) = wir::verify(&compiled.document) {
        panic!(
            "{}",
            problems
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
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
    let observer = json!({"value": {"id": 1, "feeds": []}});
    assert_eq!(
        task["captures"]["observer"], observer,
        "the process as observers see it"
    );
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
    assert_eq!(body["captures"], json!({"suffix": {"value": "!"}}));
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
    let literal = op(main, "literal.0");
    assert_eq!(
        literal["inputs"][0]["default"], "a",
        "a plain value, read as a stream"
    );
    assert_eq!(process["outputs"][0]["flow"], "stream", "at most one task");
}

#[test]
fn pins_data_reads() {
    let script = "include { samplesheetToList } from 'plugin/nf-schema'\n\nworkflow {\n    \
                  channel.fromList(samplesheetToList('sheet.csv', 'schema.json')).view()\n}\n";
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("main.nf"), script).unwrap();
    let config = "plugins {\n    id 'nf-schema@2.5.1'\n}\n";
    std::fs::write(directory.path().join("nextflow.config"), config).unwrap();
    let schema = r#"{"type": "array", "items": {"type": "object",
        "properties": {"id": {"type": "string"}}}}"#;
    std::fs::write(directory.path().join("schema.json"), schema).unwrap();
    std::fs::write(directory.path().join("sheet.csv"), "id\na\nb\n").unwrap();
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
    assert!(compiled.full(), "{:?}", compiled.kept);
    let document = json(&compiled);
    let sources = document["sources"].as_array().unwrap();
    let sheet = sources
        .iter()
        .find(|source| source["location"] == "sheet.csv");
    let digest = "sha256:55fc01feec80df5521dde64da128cf7c9a69b3dad5d275b5008f6ba671f8c915";
    assert_eq!(sheet.map(|source| &source["digest"]), Some(&json!(digest)));
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
    let path = json!({"union": [{"file": {}}, "string"]});
    assert_eq!(
        by("sheet")["type"],
        path,
        "a File, or its text as a direct run takes it"
    );
    assert_eq!(by("sheet")["default"], "sheet.csv");
    assert_eq!(
        (&by("greeting")["type"], &by("count")["type"]),
        (&json!("string"), &json!("int"))
    );
    assert!(
        wir::verify(&compiled.document).is_ok(),
        "the defaults fit their types"
    );
}

#[test]
fn publishes_workflow_outputs() {
    let outputs = "workflow {\n    main:\n    GREET(channel.of('a'))\n    publish:\n    \
                   texts = GREET.out.text\n    logs = GREET.out.text\n}\n\noutput {\n    \
                   texts {\n        path 'texts'\n        mode 'copy'\n        \
                   index { path 'texts.csv' }\n    }\n    \
                   logs {\n        path { text -> text >> 'logs/' }\n    }\n}\n";
    let document = full(&format!("{GREET}{outputs}"));
    let main = graph(&document, "main");
    let texts = op(main, "output-texts.2");
    let output = &texts["kind"]["observe"]["item"]["captures"]["output"]["value"];
    let directory = output["directory"].as_str().unwrap();
    assert!(directory.ends_with("/results"), "{directory}");
    assert_eq!(texts["effects"], json!([{"publish": directory}]));
    assert_eq!(
        (&output["mode"], &output["options"]["path"]),
        (&json!("copy"), &json!("texts"))
    );
    assert!(texts["kind"]["observe"].get("close").is_some(), "the index");
    let logs = op(main, "output-logs.3");
    let path = &logs["kind"]["observe"]["item"]["captures"]["output"]["value"]["options"]["path"];
    assert_eq!(
        (&path["class"], &path["source"]),
        (&json!("Closure"), &json!("main"))
    );
    assert!(logs["kind"]["observe"].get("close").is_none(), "no index");
}

#[test]
fn pads_join_remainders() {
    let script = "workflow {\n    channel.of([1, 'a'], [2, 'b'])\n        \
                  .join(channel.of([1, 'x']), remainder: true).view()\n}\n";
    let document = full(script);
    let main = graph(&document, "main");
    let join = op(main, "join.2");
    let shape = json!({"tuple": ["dynamic", "dynamic"]});
    assert_eq!(join["inputs"][0]["type"], shape, "the key and one list");
    assert!(join["kind"]["join"]["body"]["code"].is_string());
    let split = &op(main, "join.2/left")["kind"]["transform"]["body"]["code"];
    assert!(split.as_str().unwrap().contains("[0].collect"), "{split}");
}

#[test]
fn topics_follow_readers() {
    let script = "process A {\n    output:\n    val 'a', topic: names\n    script:\n    'true'\n}\n\n\
                  process B {\n    output:\n    val 'b', topic: names\n    script:\n    'true'\n}\n\n\
                  workflow SUB {\n    main:\n    A()\n    channel.topic('names').collect().view()\n}\n\n\
                  workflow {\n    B()\n    SUB()\n}\n";
    let document = full(script);
    let sub = graph(&document, "workflow.SUB");
    let ops = sub["ops"].as_array().unwrap();
    let mix = ops.iter().find(|op| op["kind"].get("mix").is_some());
    assert!(mix.is_some(), "the mix sits with its reader: {sub}");
}

#[test]
fn passes_single_topics() {
    let script = "process A {\n    output:\n    val 'a', topic: names\n    script:\n    'true'\n}\n\n\
                  workflow {\n    A()\n    channel.topic('names').view()\n}\n";
    let document = full(script);
    let main = graph(&document, "main");
    let ops = main["ops"].as_array().unwrap();
    let topic = ops
        .iter()
        .find(|op| op["path"].as_str().unwrap().starts_with("topic-"));
    assert_eq!(topic.map(|op| op["kind"]["filter"].is_object()), Some(true));
}

#[test]
fn pins_loaded_scripts() {
    let script =
        "include { tag } from './helper.nf'\nworkflow {\n    channel.of(tag()).view()\n}\n";
    let helper = "def tag() { 'one' }\n";
    let class = "class Util {\n    static String name() { 'util' }\n}\n";
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("main.nf"), script).unwrap();
    std::fs::write(directory.path().join("helper.nf"), helper).unwrap();
    std::fs::create_dir(directory.path().join("lib")).unwrap();
    std::fs::write(directory.path().join("lib").join("Util.groovy"), class).unwrap();
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
    let document = json(&compiled);
    let sources = document["sources"].as_array().unwrap();
    for location in ["helper.nf", "lib/Util.groovy"] {
        let source = sources.iter().find(|source| source["location"] == location);
        let digest = source.map(|source| source["digest"].as_str().unwrap_or_default());
        assert!(
            digest.is_some_and(|digest| digest.starts_with("sha256:")),
            "{location}: {sources:?}"
        );
    }
}

#[test]
fn records_observed_processes() {
    let document = full(&format!(
        "{GREET}\nworkflow {{\n    GREET(channel.of('a'))\n}}\n"
    ));
    let hooks = document["hooks"].as_array().expect("run hooks");
    let observe = &hooks[0]["observe"];
    assert_eq!(observe["source"], "main");
    let process = &observe["processes"][0];
    assert_eq!(
        (&process["name"], &process["script"]),
        (&json!("GREET"), &json!("main.nf"))
    );
}

/// Compiles `script` with `config` as its `nextflow.config`, without a profile.
fn compile_config(script: &str, config: &str) -> Compilation {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("main.nf"), script).unwrap();
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
    nextflow_compiler::compile(&launch, settings, directory.path()).unwrap()
}

#[test]
fn types_native_params() {
    let config = "params.extra = null\nparams.opts = [a: 1, b: 'x']\nparams.none = []\n\
                  params.mixed = [1, 'a']\n";
    let compiled = compile_config(
        "workflow {\n    channel.of(['a', 1]).groupBy().view()\n}\n",
        config,
    );
    assert!(
        !compiled.full(),
        "the typed operator keeps the pipeline native"
    );
    if let Err(problems) = wir::verify(&compiled.document) {
        let shown: Vec<String> = problems.iter().map(ToString::to_string).collect();
        panic!("the native document verifies: {}", shown.join("; "));
    }
    let document = json(&compiled);
    let names: Vec<&str> = (graph(&document, "main")["inputs"]
        .as_array()
        .unwrap()
        .iter())
    .map(|port| port["name"].as_str().unwrap())
    .collect();
    for name in ["extra", "opts", "none", "mixed"] {
        assert!(names.contains(&name), "{name} in {names:?}");
    }
}

#[test]
fn keeps_refused_params() {
    let script = "workflow {\n    file(\"${launchDir}/old.txt\").delete()\n}\n";
    let compiled = compile_config(script, "params.sample = 'a'\n");
    assert!(
        compiled.kept[0].reason.contains("deleting files"),
        "{:?}",
        compiled.kept
    );
    let document = json(&compiled);
    let main = graph(&document, "main");
    let param = json!({"name": "sample", "type": {"optional": "string"}, "default": "a"});
    assert_eq!(main["inputs"], json!([param]), "the params of the config");
    let edge = json!({"from": {"port": "sample"}, "to": {"op": "pipeline", "port": "sample"}});
    assert!(main["edges"].as_array().unwrap().contains(&edge));
}

#[test]
fn keeps_unbridged_params() {
    let script = "workflow {\n    file(\"${launchDir}/old.txt\").delete()\n}\n";
    let config = "params.sample = 'a'\nparams.max_time = 1.h\nparams.max_memory = 2.GB\n";
    let compiled = compile_config(script, config);
    assert!(
        compiled.kept[0].reason.contains("deleting files"),
        "{:?}",
        compiled.kept
    );
    let document = json(&compiled);
    let names: Vec<&str> = (graph(&document, "main")["inputs"]
        .as_array()
        .unwrap()
        .iter())
    .map(|port| port["name"].as_str().unwrap())
    .collect();
    assert_eq!(
        names,
        ["sample"],
        "a duration and a memory keep their config defaults"
    );
}
