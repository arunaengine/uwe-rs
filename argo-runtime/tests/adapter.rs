//! Single template calls through the adapter give the tasks, keys, acceptance, outputs and
//! results of the native run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use argo_runtime::adapter::{Adapter, CallId, Instance};
use argo_runtime::{Services, load_driver};
use serde_json::{Value as Json, json};
use task_model::{Digest, FileKind, InputFile};
use workflow_graph::{Driver, Launch, Outcome, OutputFile, Task};

/// A script that makes a text and a folder after one failed attempt, and a container that
/// reads both and its pod name, which comes from the node name.
const STEPS: &str = r#"  entrypoint: main
  templates:
  - name: main
    steps:
    - - name: make
        template: make
        arguments:
          parameters: [{name: word, value: hello}]
    - - name: use
        template: use
        arguments:
          parameters: [{name: text, value: "{{steps.make.outputs.parameters.text}}"}]
          artifacts: [{name: data, from: "{{steps.make.outputs.artifacts.data}}"}]
    outputs:
      parameters:
      - {name: text, valueFrom: {parameter: "{{steps.use.outputs.parameters.text}}"}}
  - name: make
    inputs:
      parameters: [{name: word}]
    retryStrategy: {limit: 2}
    script:
      image: alpine:3.20
      command: [sh]
      source: |
        echo {{inputs.parameters.word}} > /tmp/text
        mkdir -p /tmp/data
    outputs:
      parameters: [{name: text, valueFrom: {path: /tmp/text}}]
      artifacts: [{name: data, path: /tmp/data}]
  - name: use
    inputs:
      parameters: [{name: text}]
      artifacts: [{name: data, path: /in/data}]
    container:
      image: alpine:3.20
      command: [sh, -c]
      args: ["ls /in/data; echo {{inputs.parameters.text}} {{pod.name}} > /tmp/out"]
    outputs:
      parameters: [{name: text, valueFrom: {path: /tmp/out}}]
"#;

const DAEMON: &str = r#"  entrypoint: main
  templates:
  - name: main
    dag:
      tasks:
      - {name: server, template: server}
      - {name: client, template: client, depends: server}
  - name: server
    daemon: true
    container: {image: nginx:1.27}
  - name: client
    container: {image: alpine:3.20, command: [echo, ready]}
"#;

fn launch(folder: &Path, spec: &str) -> Launch {
    let target = folder.join("workflow.yaml");
    let text = format!(
        "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {{name: parity}}\nspec:\n{spec}"
    );
    std::fs::write(&target, text).unwrap();
    Launch {
        target,
        scratch: folder.join("scratch"),
        ..Launch::default()
    }
}

/// An outcome of a task with its exit code and output files; a path that ends with `/` is a
/// folder.
fn outcome(folder: &Path, task: &Task, attempt: u32, code: i32, paths: &[(&str, &str)]) -> Outcome {
    let files = paths
        .iter()
        .map(|(path, text)| {
            let name = format!("{}-{}", task.id, path.trim_matches('/').replace('/', "-"));
            let location = folder.join(name);
            let directory = path.ends_with('/');
            match directory {
                true => std::fs::create_dir_all(&location).unwrap(),
                false => std::fs::write(&location, text).unwrap(),
            }
            OutputFile {
                path: path.trim_end_matches('/').into(),
                location: location.to_string_lossy().into_owned(),
                directory,
                ..OutputFile::default()
            }
        })
        .collect();
    Outcome {
        id: task.id,
        attempt,
        exit_code: code,
        stdout: "printed\n".into(),
        files,
        ..Outcome::default()
    }
}

/// What the parity tests compare of a task: all its fields apart from the tasks it waits for,
/// its task cache key with an input digest per source, and its resume fingerprint.
fn described(task: &Task) -> (Json, Digest, String) {
    let mut task = task.clone();
    task.after.clear();
    let inputs = (task.inputs.iter())
        .map(|input| InputFile {
            path: input.path.clone(),
            kind: match input.directory {
                true => FileKind::Directory,
                false => FileKind::File,
            },
            digest: Digest::sha256(input.source.as_bytes()),
            executable: input.executable,
            writable: false,
        })
        .collect();
    let request = task_runner::request(&task, task.image.clone(), inputs, None);
    let key = request.spec.cache_key().unwrap();
    let fingerprint = run_database::fingerprint(&task, &[]);
    (serde_json::to_value(&task).unwrap(), key, fingerprint)
}

fn assert_same(native: &[Task], adapted: &[Task]) {
    assert_eq!(native.len(), adapted.len(), "as many tasks");
    for (one, other) in native.iter().zip(adapted) {
        assert_eq!(described(one), described(other), "task {}", one.name);
    }
}

fn instance(name: &str, template: &str, parameters: &[(&str, Json)]) -> Instance {
    let text = |value: &Json| value.as_str().unwrap_or_default().to_owned();
    Instance {
        name: name.into(),
        template: template.into(),
        parameters: (parameters.iter())
            .filter(|(_, value)| value.is_string())
            .map(|(name, value)| ((*name).into(), text(value)))
            .collect(),
        artifacts: (parameters.iter())
            .filter(|(_, value)| value.is_object())
            .map(|(name, value)| ((*name).into(), value.clone()))
            .collect(),
        ..Instance::default()
    }
}

#[test]
fn native_task_keys() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let launch = launch(root, STEPS);
    let mut native = load_driver(&launch, Services::default()).unwrap();
    let mut adapter = Adapter::load(&launch, Services::default()).unwrap();
    let files: PathBuf = root.join("files");
    std::fs::create_dir_all(&files).unwrap();
    let made = [("/tmp/text", "hello\n"), ("/tmp/data/", "")];

    let first = native.start().unwrap();
    let (make, adapted) = adapter
        .start(&instance("[0].make", "make", &[("word", json!("hello"))]))
        .unwrap();
    assert_same(&first, &adapted);
    let failed = outcome(&files, &first[0], 1, 1, &[]);
    let second = native.complete(failed.clone()).unwrap();
    let adapted = adapter.complete(failed).unwrap();
    assert_same(&second, &adapted);
    assert!(adapter.ending(make).is_none(), "the retry still runs");
    let done = outcome(&files, &second[0], 1, 0, &made);
    let third = native.complete(done.clone()).unwrap();
    assert!(adapter.complete(done).unwrap().is_empty());
    let ending = adapter.ending(make).unwrap();
    assert!(ending.facts.succeeded);
    let given = [
        ("text", ending.outputs["parameter/text"].clone()),
        ("data", ending.outputs["artifact/data"].clone()),
    ];
    let (_, adapted) = adapter.start(&instance("[1].use", "use", &given)).unwrap();
    assert_same(&third, &adapted);
}

#[test]
fn native_acceptance() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let launch = launch(root, STEPS);
    let mut native = load_driver(&launch, Services::default()).unwrap();
    let mut adapter = Adapter::load(&launch, Services::default()).unwrap();
    let made = [("/tmp/text", "hello\n"), ("/tmp/data/", "")];
    let task = native.start().unwrap().remove(0);
    let (make, _) = adapter
        .start(&instance("[0].make", "make", &[("word", json!("hello"))]))
        .unwrap();
    let failed = outcome(root, &task, 1, 1, &[]);
    let retry = native.complete(failed.clone()).unwrap().remove(0);
    adapter.complete(failed).unwrap();
    assert!(!native.take_acceptance(task.id, 1));
    assert!(!adapter.take_acceptance(task.id, 1), "a failed attempt");
    let done = outcome(root, &retry, 1, 0, &made);
    let used = native.complete(done.clone()).unwrap().remove(0);
    adapter.complete(done).unwrap();
    assert!(native.take_acceptance(retry.id, 1));
    assert!(
        adapter.take_acceptance(retry.id, 1),
        "the attempt that made the outputs"
    );
    let ending = adapter.ending(make).unwrap();
    assert_eq!(ending.outputs["parameter/text"], json!("hello"));
    assert_eq!(ending.outputs["result"], json!("printed"));
    assert_eq!(ending.outputs["exitCode"], json!("0"));
    assert_eq!(ending.outputs["artifact/data"]["class"], json!("Directory"));
    let given = [
        ("text", ending.outputs["parameter/text"].clone()),
        ("data", ending.outputs["artifact/data"].clone()),
    ];
    let (used_call, tasks) = adapter.start(&instance("[1].use", "use", &given)).unwrap();
    let out = [("/tmp/out", "hello parity-use-1\n")];
    let last = outcome(root, &tasks[0], 1, 0, &out);
    assert!(
        native
            .complete(outcome(root, &used, 1, 0, &out))
            .unwrap()
            .is_empty()
    );
    adapter.complete(last).unwrap();
    let ending = adapter.ending(used_call).unwrap();
    let exported = native.exported().unwrap();
    assert_eq!(ending.outputs["parameter/text"], exported["text"]);
    let mut again = Adapter::load(&launch, Services::default()).unwrap();
    let (call, tasks) = again.start(&instance("[1].use", "use", &given)).unwrap();
    again.complete(outcome(root, &tasks[0], 1, 0, &[])).unwrap();
    let ending = again.ending(call).unwrap();
    assert!(ending.facts.errored, "an output parameter without its file");
}

#[test]
fn daemon_control() {
    let folder = tempfile::tempdir().unwrap();
    let launch = launch(folder.path(), DAEMON);
    let mut native = load_driver(&launch, Services::default()).unwrap();
    let mut adapter = Adapter::load(&launch, Services::default()).unwrap();
    let started = native.start().unwrap();
    assert_eq!(
        started.len(),
        1,
        "the client waits for the daemon to be ready"
    );
    let (server, adapted): (CallId, Vec<Task>) =
        adapter.start(&instance(".server", "server", &[])).unwrap();
    assert_same(&started, &adapted);
    let facts = adapter.facts(server);
    assert!(
        facts.daemoned && !facts.succeeded,
        "daemoned before its outcome"
    );
    assert!(
        adapter.ending(server).is_none(),
        "no control result before readiness"
    );
    let ready = Outcome {
        id: adapted[0].id,
        attempt: 1,
        address: Some("10.0.0.7".into()),
        ..Outcome::default()
    };
    let client = native.complete(ready.clone()).unwrap();
    assert_eq!(client.len(), 1, "readiness starts the client");
    assert!(adapter.complete(ready).unwrap().is_empty());
    let ending = adapter.ending(server).unwrap();
    assert!(ending.facts.daemoned && ending.facts.succeeded);
    assert!(adapter.take_acceptance(adapted[0].id, 1));
}

/// A loop over the result of a step, a loop over a sequence, and a step that reads the
/// results of the first loop, as JSON.
const LOOPS: &str = r#"  entrypoint: main
  templates:
  - name: main
    steps:
    - - {name: gen, template: say, arguments: {parameters: [{name: text, value: start}]}}
    - - name: each
        template: say
        withParam: "{{steps.gen.outputs.result}}"
        when: "{{steps.gen.outputs.result}} != none"
        arguments: {parameters: [{name: text, value: "{{item.name}}-{{workflow.name}}"}]}
      - name: count
        template: say
        withSequence: {count: "2", format: "n%02d"}
        arguments: {parameters: [{name: text, value: "{{item}}"}]}
    - - {name: last, template: say, arguments: {parameters: [{name: text, value: "{{steps.each.outputs.result}}"}]}}
  - name: say
    inputs: {parameters: [{name: text}]}
    container: {image: alpine:3.20, command: [echo, "{{inputs.parameters.text}}"]}
"#;

#[test]
fn lowered_expressions() {
    use argo_runtime::adapter::Form;
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let launch = launch(root, LOOPS);
    let mut native = load_driver(&launch, Services::default()).unwrap();
    let mut adapter = Adapter::load(&launch, Services::default()).unwrap();
    let first = native.start().unwrap().remove(0);
    let mut printed = outcome(root, &first, 1, 0, &[]);
    let list = r#"[{"name": "a"}, {"name": "b(1)"}]"#;
    printed.stdout = list.into();
    let looped = native.complete(printed).unwrap();
    let names: Vec<&str> = looped.iter().map(|task| task.name.as_str()).collect();
    let commands: Vec<&str> = looped.iter().map(|task| task.command[1].as_str()).collect();
    let values = serde_json::Map::from_iter([("steps.gen.outputs.result".into(), json!(list))]);
    let mut evaluate = |text: &str, form, site: &str, values: &serde_json::Map<String, Json>| {
        adapter.evaluate(text, form, site, values).unwrap()
    };
    let text = "{{steps.gen.outputs.result}}";
    let items = evaluate(text, Form::Items, "[1].each", &values);
    assert_eq!(items, json!([{"name": "a"}, {"name": "b(1)"}]));
    let labels = evaluate(text, Form::Labels, "[1].each", &values);
    let sequence = r#"{"count": "2", "format": "n%02d"}"#;
    let counted = evaluate(sequence, Form::Items, "[1].count", &serde_json::Map::new());
    let counts = evaluate(sequence, Form::Labels, "[1].count", &serde_json::Map::new());
    let mut expected = Vec::new();
    for (index, item) in items.as_array().unwrap().iter().enumerate() {
        let label = labels[index].as_str().unwrap();
        let mut values = values.clone();
        values.insert("item".into(), item.clone());
        let when = "{{steps.gen.outputs.result}} != none";
        let decided = evaluate(when, Form::Condition, "[1].each", &values);
        assert_eq!(decided, json!(true));
        let text = evaluate("{{item.name}}-{{workflow.name}}", Form::Text, "", &values);
        expected.push((format!("parity[1].each{label}"), text));
    }
    for (index, item) in counted.as_array().unwrap().iter().enumerate() {
        let values = serde_json::Map::from_iter([("item".into(), item.clone())]);
        let text = evaluate("{{item}}", Form::Text, "", &values);
        let label = counts[index].as_str().unwrap();
        expected.push((format!("parity[1].count{label}"), text));
    }
    let found: Vec<(String, Json)> = (names.iter().zip(&commands))
        .map(|(name, command)| ((*name).to_owned(), json!(command)))
        .collect();
    assert_eq!(found, expected, "node names and arguments of the loops");
    let mut results = Vec::new();
    let mut next = Vec::new();
    for (position, task) in looped.iter().enumerate() {
        let mut done = outcome(root, task, 1, 0, &[]);
        done.stdout = format!("{{\"n\": {position}}}\n");
        results.push(json!(done.stdout.trim_end()));
        next = native.complete(done).unwrap();
    }
    results.truncate(2);
    let values = serde_json::Map::from_iter([("steps.each.outputs.result".into(), json!(results))]);
    let text = evaluate("{{steps.each.outputs.result}}", Form::Text, "", &values);
    assert_eq!(
        json!(next[0].command[1]),
        text,
        "the results of a loop as JSON"
    );
}
