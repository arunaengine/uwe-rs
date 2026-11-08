//! Compiles small Argo workflows into wir and checks the graphs, dialect tasks and control.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::{Value as Json, json};

fn compiled(folder: &Path, spec: &str) -> (Json, Vec<argo_compiler::Kept>) {
    let target = folder.join("workflow.yaml");
    let text = format!(
        "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {{name: compiled}}\nspec:\n{spec}"
    );
    std::fs::write(&target, text).unwrap();
    let compiled = argo_compiler::compile(&target, &[], folder).unwrap();
    let json = serde_json::to_value(&compiled.document).unwrap();
    (json, compiled.kept)
}

fn graph<'d>(document: &'d Json, name: &str) -> &'d Json {
    let graphs = document["graphs"].as_array().unwrap();
    graphs
        .iter()
        .find(|graph| graph["name"] == name)
        .unwrap_or_else(|| panic!("graph {name}"))
}

fn op<'g>(graph: &'g Json, path: &str) -> &'g Json {
    let ops = graph["ops"].as_array().unwrap();
    ops.iter()
        .find(|op| op["path"] == path)
        .unwrap_or_else(|| panic!("op {path}"))
}

const LEAF: &str = r#"  entrypoint: say
  arguments: {parameters: [{name: text, value: hello}]}
  templates:
  - name: say
    inputs: {parameters: [{name: text}]}
    container: {image: alpine:3.20, command: [echo, "{{inputs.parameters.text}}"]}
    outputs: {parameters: [{name: out, valueFrom: {path: /tmp/out}}]}
"#;

#[test]
fn compiles_leaf_entry() {
    let folder = tempfile::tempdir().unwrap();
    let (document, kept) = compiled(folder.path(), LEAF);
    assert!(kept.is_empty(), "{kept:?}");
    let entry = graph(&document, "workflow");
    assert_eq!(
        entry["inputs"][0],
        json!({"name": "text", "type": "string", "default": "hello"})
    );
    let task = &op(entry, "main")["kind"]["task"]["builder"]["argo_task"];
    assert_eq!(task["entry"], "say");
    assert_eq!(task["call"], "entrypoint");
    let outputs: Vec<&Json> = (task["outputs"].as_array().unwrap().iter())
        .map(|map| &map["native"])
        .collect();
    let expected = [
        "parameter/outputs.parameters.out",
        "parameter/outputs.result",
        "parameter/exitCode",
    ];
    assert_eq!(outputs, expected.map(Json::from).iter().collect::<Vec<_>>());
    assert_eq!(entry["outputs"][0]["name"], "out");
    let container =
        r#"container: {image: alpine:3.20, command: [echo, "{{inputs.parameters.text}}"]}"#;
    let suspend = LEAF.replace(container, "suspend: {}");
    let (document, kept) = compiled(folder.path(), &suspend);
    assert!(kept[0].reason.contains("suspend"), "{kept:?}");
    let whole = &op(graph(&document, "workflow"), "workflow")["kind"]["native"];
    assert_eq!(whole["language"], "argo");
}

const STEPS: &str = r#"  entrypoint: main
  arguments: {parameters: [{name: word, value: hello}]}
  templates:
  - name: main
    inputs: {parameters: [{name: word}]}
    steps:
    - - {name: make, template: make, arguments: {parameters: [{name: word, value: "{{inputs.parameters.word}}"}]}}
    - - name: use
        template: use
        continueOn: {failed: true}
        arguments:
          parameters: [{name: text, value: "{{steps.make.outputs.parameters.text}}"}]
          artifacts: [{name: data, from: "{{steps.make.outputs.artifacts.data}}"}]
      - {name: inner, template: inner, arguments: {parameters: [{name: text, value: fixed}]}}
    outputs:
      parameters: [{name: text, valueFrom: {parameter: "{{steps.use.outputs.parameters.text}}"}}]
  - name: inner
    inputs: {parameters: [{name: text}]}
    steps:
    - - {name: each, template: use, withItems: [a, b], arguments: {parameters: [{name: text, value: "{{item}}"}]}}
  - name: make
    inputs: {parameters: [{name: word}]}
    retryStrategy: {limit: 2}
    script: {image: alpine:3.20, command: [sh], source: "echo {{inputs.parameters.word}} > /tmp/text"}
    outputs:
      parameters: [{name: text, valueFrom: {path: /tmp/text}}]
      artifacts: [{name: data, path: /tmp/data}]
  - name: use
    inputs:
      parameters: [{name: text}]
      artifacts: [{name: data, path: /in/data, optional: true}]
    container: {image: alpine:3.20, command: [echo, "{{inputs.parameters.text}}"]}
    outputs:
      parameters: [{name: text, valueFrom: {path: /tmp/out}}]
"#;

#[test]
fn compiles_steps() {
    let folder = tempfile::tempdir().unwrap();
    let (document, kept) = compiled(folder.path(), STEPS);
    assert!(kept.is_empty(), "{kept:?}");
    let entry = graph(&document, "workflow");
    assert_eq!(op(entry, "main")["kind"]["call"]["graph"], "template.main");
    assert_eq!(entry["outputs"][0]["name"], "text");
    let main = graph(&document, "template.main");
    let make = &op(main, "make")["kind"]["task"]["builder"]["argo_task"];
    assert_eq!(make["entry"], "main");
    assert_eq!(make["call"], "[0].make");
    let natives: Vec<&Json> = (make["inputs"].as_array().unwrap().iter())
        .map(|map| &map["native"])
        .collect();
    assert_eq!(
        natives,
        [
            &json!("parameter/node.path"),
            &json!("parameter/inputs.parameters.word")
        ]
    );
    let used = op(main, "use");
    assert_eq!(
        used["after"],
        json!([{"op": "make", "on": ["succeeded", "skipped"]}])
    );
    let ports: Vec<&Json> = (used["inputs"].as_array().unwrap().iter())
        .map(|port| &port["name"])
        .collect();
    assert!(ports.contains(&&json!("steps.make.outputs.artifacts.data")));
    let handler = op(main, "use.continue");
    assert_eq!(handler["handles"], json!([{"op": "use", "on": ["failed"]}]));
    assert_eq!(op(main, "inner.path")["kind"], json!({"flatten": {}}));
    let inner = graph(&document, "template.inner");
    let each = &op(inner, "each")["kind"]["map"];
    assert_eq!(each["over"], json!(["item"]));
    let body = graph(&document, "template.inner.each");
    let task = &op(body, "each")["kind"]["task"]["builder"]["argo_task"];
    assert_eq!(task["entry"], "inner");
    let round = &op(inner, "each")["inputs"];
    let items = round
        .as_array()
        .unwrap()
        .iter()
        .find(|port| port["name"] == "item")
        .unwrap();
    assert_eq!(items["default"], json!(["a", "b"]));
}

const DAG: &str = r#"  entrypoint: main
  templates:
  - name: main
    dag:
      tasks:
      - {name: a, template: say}
      - {name: b, template: say, depends: "a.Failed"}
      - {name: c, template: say, depends: "a || b"}
      - {name: lone, template: say}
      target: c
  - name: say
    container: {image: alpine:3.20, command: [echo, hi]}
"#;

#[test]
fn compiles_dag_control() {
    let folder = tempfile::tempdir().unwrap();
    let (document, kept) = compiled(folder.path(), DAG);
    assert!(kept.is_empty(), "{kept:?}");
    let main = graph(&document, "template.main");
    let paths: Vec<&Json> = main["ops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|op| &op["path"])
        .collect();
    assert!(
        !paths.contains(&&json!("lone")),
        "a task that the target does not need"
    );
    let b = op(main, "b");
    assert_eq!(
        b["depends"],
        json!({"result": {"op": "a", "on": ["failed"]}})
    );
    assert_eq!(b["handles"], json!([{"op": "a", "on": ["failed"]}]));
    let bare = json!(["succeeded", "skipped", "daemoned"]);
    let expected =
        json!({"any": [{"result": {"op": "a", "on": bare}}, {"result": {"op": "b", "on": bare}}]});
    assert_eq!(op(main, "c")["depends"], expected);
    assert!(op(main, "c").get("handles").is_none());
}

#[test]
fn keeps_native_reasons() {
    let folder = tempfile::tempdir().unwrap();
    let suspend = DAG.replace(
        "container: {image: alpine:3.20, command: [echo, hi]}",
        "suspend: {duration: 1s}",
    );
    let (_, kept) = compiled(folder.path(), &suspend);
    assert!(kept[0].reason.contains("suspend"), "{kept:?}");
}

const SEQUENCE: &str = r#"  entrypoint: main
  arguments: {parameters: [{name: word, value: hello}]}
  templates:
  - name: main
    inputs: {parameters: [{name: word}]}
    steps:
    - - {name: make, template: make, arguments: {parameters: [{name: word, value: "{{inputs.parameters.word}}"}]}}
    - - name: use
        template: use
        continueOn: {failed: true}
        arguments:
          parameters: [{name: text, value: "{{steps.make.outputs.parameters.text}}"}]
          artifacts: [{name: data, from: "{{steps.make.outputs.artifacts.data}}"}]
    outputs:
      parameters: [{name: text, valueFrom: {parameter: "{{steps.use.outputs.parameters.text}}"}}]
  - name: make
    inputs: {parameters: [{name: word}]}
    retryStrategy: {limit: 2}
    script: {image: alpine:3.20, command: [sh], source: "echo {{inputs.parameters.word}} > /tmp/text"}
    outputs:
      parameters: [{name: text, valueFrom: {path: /tmp/text}}]
      artifacts: [{name: data, path: /tmp/data}]
  - name: use
    inputs:
      parameters: [{name: text}]
      artifacts: [{name: data, path: /in/data, optional: true}]
    container: {image: alpine:3.20, command: [echo, "{{inputs.parameters.text}}"]}
    outputs:
      parameters: [{name: text, valueFrom: {path: /tmp/out}}]
"#;

#[test]
fn compiles_step_groups() {
    let folder = tempfile::tempdir().unwrap();
    let (document, kept) = compiled(folder.path(), SEQUENCE);
    assert!(kept.is_empty(), "{kept:?}");
    let entry = graph(&document, "workflow");
    assert_eq!(op(entry, "main")["kind"]["call"]["graph"], "template.main");
    assert_eq!(entry["outputs"][0]["name"], "text");
    let main = graph(&document, "template.main");
    let make = &op(main, "make")["kind"]["task"]["builder"]["argo_task"];
    assert_eq!(make["entry"], "main");
    assert_eq!(make["call"], "[0].make");
    let natives: Vec<&Json> = (make["inputs"].as_array().unwrap().iter())
        .map(|map| &map["native"])
        .collect();
    let expected = [
        json!("parameter/node.path"),
        json!("parameter/inputs.parameters.word"),
    ];
    assert_eq!(natives, expected.iter().collect::<Vec<_>>());
    let used = op(main, "use");
    assert_eq!(
        used["after"],
        json!([{"op": "make", "on": ["succeeded", "skipped"]}])
    );
    let ports: Vec<&Json> = (used["inputs"].as_array().unwrap().iter())
        .map(|port| &port["name"])
        .collect();
    assert!(ports.contains(&&json!("steps.make.outputs.artifacts.data")));
    let handler = op(main, "use.continue");
    assert_eq!(handler["handles"], json!([{"op": "use", "on": ["failed"]}]));
    let output = (main["edges"].as_array().unwrap().iter())
        .find(|edge| edge["to"]["port"] == "outputs.parameters.text");
    assert_eq!(
        output.unwrap()["from"],
        json!({"op": "use", "port": "outputs.parameters.text"})
    );
}

const LOOPS: &str = r#"  entrypoint: main
  templates:
  - name: main
    steps:
    - - {name: each, template: say, withItems: [a, {x: 1}], arguments: {parameters: [{name: text, value: "{{item}}"}]}}
      - {name: count, template: say, withSequence: {count: "2"}, arguments: {parameters: [{name: text, value: "{{item}}"}]}}
  - name: say
    inputs: {parameters: [{name: text}]}
    container: {image: alpine:3.20, command: [echo, "{{inputs.parameters.text}}"]}
"#;

#[test]
fn compiles_static_loops() {
    let folder = tempfile::tempdir().unwrap();
    let (document, kept) = compiled(folder.path(), LOOPS);
    assert!(kept.is_empty(), "{kept:?}");
    let main = graph(&document, "template.main");
    let each = op(main, "each");
    assert_eq!(
        each["kind"]["map"],
        json!({"body": "template.main.each", "over": ["item"]})
    );
    let limited = LOOPS
        .replace("    steps:\n", "    parallelism: 2\n    steps:\n")
        .replace("\n      - {name: count", "\n    - - {name: count");
    let (limited, kept) = compiled(folder.path(), &limited);
    assert!(kept.is_empty(), "{kept:?}");
    let each_map = &op(graph(&limited, "template.main"), "each")["kind"]["map"];
    assert_eq!(each_map["parallelism"], 2);
    let item = |op: &Json| {
        op["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|port| port["name"] == "item")
            .cloned()
    };
    assert_eq!(item(each).unwrap()["default"], json!(["a", {"x": 1}]));
    assert_eq!(
        item(op(main, "count")).unwrap()["default"],
        json!(["0", "1"])
    );
    let body = graph(&document, "template.main.each");
    let task = &op(body, "each")["kind"]["task"]["builder"]["argo_task"];
    assert_eq!(task["call"], "[0].each");
    let maps = task["inputs"].as_array().unwrap();
    assert!(maps.iter().any(|map| map["native"] == "parameter/item"));
    let result =
        (each["outputs"].as_array().unwrap().iter()).find(|port| port["name"] == "outputs.result");
    assert_eq!(
        result.unwrap()["type"],
        json!({"array": {"items": {"optional": "string"}}})
    );
    let param = LOOPS.replace("withItems: [a, {x: 1}]", "withParam: '[\"a\"]'");
    let (document, kept) = compiled(folder.path(), &param);
    assert!(kept.is_empty(), "{kept:?}");
    let items = op(graph(&document, "template.main"), "each.items");
    let expr = json!({"language": "argo", "code": "[\"a\"]", "form": "items"});
    assert_eq!(items["kind"]["expr"], expr);
}

const TEXTS: &str = r#"  entrypoint: main
  arguments: {parameters: [{name: word, value: hi}]}
  templates:
  - name: main
    steps:
    - - {name: gen, template: say}
    - - name: each
        template: inner
        withParam: "{{steps.gen.outputs.result}}"
        when: "{{item.x}} == 1"
        arguments: {parameters: [{name: text, value: "{{item.x}}-{{workflow.parameters.word}}"}]}
    outputs:
      parameters: [{name: joined, valueFrom: {expression: "steps.each.outputs.parameters.text"}}]
  - name: inner
    inputs: {parameters: [{name: text}]}
    steps:
    - - {name: say, template: say, arguments: {parameters: [{name: text, value: "{{inputs.parameters.text}}"}]}}
    outputs:
      parameters: [{name: text, valueFrom: {parameter: "{{steps.say.outputs.result}}"}}]
  - name: say
    inputs: {parameters: [{name: text, value: x}]}
    container: {image: alpine:3.20, command: [echo, "{{inputs.parameters.text}}"]}
"#;

#[test]
fn compiles_argo_texts() {
    let folder = tempfile::tempdir().unwrap();
    let (document, kept) = compiled(folder.path(), TEXTS);
    assert!(kept.is_empty(), "{kept:?}");
    let main = graph(&document, "template.main");
    let each = op(main, "each");
    assert_eq!(each["kind"]["map"]["over"], json!(["item", "label"]));
    let items = op(main, "each.items");
    assert_eq!(items["inputs"][0]["name"], "steps.gen.outputs.result");
    assert_eq!(op(main, "each.labels")["kind"]["expr"]["form"], "labels");
    let body = graph(&document, "template.main.each");
    let guarded = &op(body, "each")["kind"]["branch"];
    assert_eq!(guarded["body"], "template.main.each.when");
    assert_eq!(op(body, "each.when")["kind"]["expr"]["form"], "condition");
    let inner = graph(&document, "template.main.each.when");
    let text = op(inner, "each.arguments.text");
    assert_eq!(text["inputs"][0]["name"], "item");
    let joined = op(main, "outputs.parameters.joined");
    assert_eq!(
        joined["inputs"][0]["type"]["array"]["items"]["optional"],
        "string"
    );
    let launch = workflow_graph::Launch {
        target: folder.path().join("workflow.yaml"),
        simulated: true,
        ..Default::default()
    };
    let services = argo_runtime::Services::default();
    let mut adapter = argo_runtime::adapter::Adapter::load(&launch, services).unwrap();
    let mut evaluate = |op: &Json, values: Json| {
        let code = op["kind"]["expr"]["code"].as_str().unwrap();
        let form = match op["kind"]["expr"]["form"].as_str() {
            Some("condition") => argo_runtime::adapter::Form::Condition,
            Some("items") => argo_runtime::adapter::Form::Items,
            Some("labels") => argo_runtime::adapter::Form::Labels,
            _ => argo_runtime::adapter::Form::Text,
        };
        let values = values.as_object().unwrap().clone();
        adapter.evaluate(code, form, "[1].each", &values).unwrap()
    };
    let list = json!({"steps.gen.outputs.result": "[{\"x\": 1}, {\"x\": 2}]"});
    assert_eq!(evaluate(items, list.clone()), json!([{"x": 1}, {"x": 2}]));
    let labels = evaluate(op(main, "each.labels"), list);
    assert_eq!(labels, json!(["(0:x:1)", "(1:x:2)"]));
    let when = op(body, "each.when");
    assert_eq!(evaluate(when, json!({"item": {"x": 2}})), json!(false));
    assert_eq!(evaluate(text, json!({"item": {"x": 1}})), json!("1-hi"));
    let outputs = json!({"steps.each.outputs.parameters.text": ["a", null, "b"]});
    assert_eq!(evaluate(joined, outputs), json!("[\"a\",\"b\"]"));
}

const RECURSION: &str = r#"  entrypoint: main
  templates:
  - name: main
    steps:
    - - {name: start, template: loop, arguments: {parameters: [{name: count, value: "0"}]}}
  - name: loop
    inputs: {parameters: [{name: count}, {name: limit, value: "3"}]}
    steps:
    - - {name: add, template: add, arguments: {parameters: [{name: count, value: "{{inputs.parameters.count}}"}]}}
    - - {name: say, template: add, arguments: {parameters: [{name: count, value: "1"}]}}
      - name: again
        template: loop
        when: "{{steps.add.outputs.result}} < {{inputs.parameters.limit}}"
        arguments: {parameters: [{name: count, value: "{{steps.add.outputs.result}}"}]}
  - name: add
    inputs: {parameters: [{name: count}]}
    script: {image: alpine:3.20, command: [sh], source: "echo $(({{inputs.parameters.count}} + 1))"}
"#;

#[test]
fn compiles_recursion_loops() {
    let folder = tempfile::tempdir().unwrap();
    let (document, kept) = compiled(folder.path(), RECURSION);
    assert!(kept.is_empty(), "{kept:?}");
    let start = &op(graph(&document, "template.main"), "start")["kind"]["loop"];
    assert_eq!(start["body"], "template.loop");
    assert_eq!(start["condition"], "recurse");
    let carried: Vec<&Json> = (start["carry"].as_array().unwrap().iter())
        .map(|carry| &carry["input"])
        .collect();
    let expected = [
        "node.path",
        "inputs.parameters.count",
        "inputs.parameters.limit",
    ];
    assert_eq!(carried, expected.map(Json::from).iter().collect::<Vec<_>>());
    let body = graph(&document, "template.loop");
    let when = op(body, "again.when");
    assert_eq!(when["kind"]["expr"]["form"], "condition");
    assert_eq!(
        op(body, "again.inputs.parameters.limit")["kind"]["expr"]["code"],
        "3"
    );
    let names: Vec<&Json> = (body["outputs"].as_array().unwrap().iter())
        .map(|port| &port["name"])
        .collect();
    assert!(
        names.contains(&&json!("recurse.inputs.parameters.count")),
        "{names:?}"
    );
    let again = "value: \"{{steps.add.outputs.result}}\"}]}\n";
    let late = "    - - {name: late, template: add, arguments: {parameters: [{name: count, value: \"1\"}]}}\n";
    let later = RECURSION.replace(again, &format!("{again}{late}"));
    let (_, kept) = compiled(folder.path(), &later);
    assert!(kept[0].reason.contains("after"), "{kept:?}");
}

const HOOKS: &str = r#"  entrypoint: main
  onExit: bye
  templates:
  - name: main
    steps:
    - - name: hello
        template: say
        hooks:
          exit:
            template: say
            arguments: {parameters: [{name: text, value: "{{steps.hello.outputs.result}}"}]}
    - - {name: next, template: say}
  - name: say
    inputs: {parameters: [{name: text, value: hi}]}
    container: {image: alpine:3.20, command: [echo, "{{inputs.parameters.text}}"]}
  - name: bye
    container: {image: alpine:3.20, command: [echo, bye]}
"#;

#[test]
fn compiles_exit_hooks() {
    let folder = tempfile::tempdir().unwrap();
    let (document, kept) = compiled(folder.path(), HOOKS);
    assert!(kept.is_empty(), "{kept:?}");
    let main = graph(&document, "template.main");
    let hook = op(main, "hello.onExit");
    assert_eq!(
        hook["kind"]["task"]["builder"]["argo_task"]["call"],
        "[0].hello.onExit"
    );
    assert_eq!(hook["after"][0]["op"], "hello");
    let handled = &op(main, "hello.onExit.continue")["kind"]["status_group"];
    assert_eq!(handled["allow"], json!(["failed", "errored"]));
    let waits: Vec<&Json> = (op(main, "next")["after"].as_array().unwrap().iter())
        .map(|after| &after["op"])
        .collect();
    assert_eq!(waits, [&json!("hello"), &json!("hello.onExit")]);
    let entry = graph(&document, "workflow");
    let exit = op(entry, "onExit");
    assert_eq!(
        exit["kind"]["task"]["builder"]["argo_task"]["call"],
        ".onExit"
    );
    assert_eq!(exit["after"][0]["op"], "main");
    assert_eq!(exit["inputs"][0]["default"], json!([]));
    let status = HOOKS.replace("echo, bye", "echo, \"{{workflow.status}}\"");
    let (_, kept) = compiled(folder.path(), &status);
    assert!(kept[0].reason.contains("workflow.status"), "{kept:?}");
}

/// The names of the tasks of a run on the simulator, sorted.
fn ran_tasks(driver: &mut dyn workflow_graph::Driver, folder: &Path) -> Vec<String> {
    let mut simulator = workflow_graph::Simulator::new(folder);
    let execution = workflow_graph::execute("compiled", driver, &mut simulator);
    let graph = execution.into_result().unwrap();
    let mut names: Vec<String> = graph.nodes.into_iter().map(|node| node.task.name).collect();
    names.sort();
    names
}

#[test]
fn runs_compiled_texts() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let looped = TEXTS.replace(
        "withParam: \"{{steps.gen.outputs.result}}\"",
        "withItems: [{x: 1}, {x: 2}]",
    );
    let (document, kept) = compiled(root, &looped);
    assert!(kept.is_empty(), "{kept:?}");
    let target = root.join("compiled.wir.json");
    std::fs::write(&target, document.to_string()).unwrap();
    let launch = |target: &Path| workflow_graph::Launch {
        target: target.to_owned(),
        scratch: root.join("scratch"),
        simulated: true,
        ..Default::default()
    };
    let services = argo_runtime::Services::default();
    let mut native =
        argo_runtime::load_driver(&launch(&root.join("workflow.yaml")), services).unwrap();
    let expected = ran_tasks(&mut native, &root.join("native"));
    let settings = wir_runtime::Settings {
        print: false,
        adapters: wir_runtime::argo::register(Default::default(), Default::default()),
        ..Default::default()
    };
    let mut wir = wir_runtime::WirDriver::new(&launch(&target), settings).unwrap();
    assert_eq!(ran_tasks(&mut wir, &root.join("wir")), expected);
    assert_eq!(expected.len(), 2, "{expected:?}");
}
