//! Runs small Argo workflows natively and compiled into wir, and compares what the runs did:
//! their tasks, how they ended and the files they delivered.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use workflow_graph::{Attempt, Driver, Executor, Launch, Outcome, OutputFile, Running, execute};

/// Writes the Argo workflow of `spec` into `root`.
fn workflow(root: &Path, spec: &str) -> PathBuf {
    let target = root.join("workflow.yaml");
    let text = format!(
        "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {{name: runs}}\nspec:\n{spec}"
    );
    std::fs::write(&target, text).unwrap();
    target
}

/// Compiles a workflow fully into wir, next to it.
fn compiled(file: &Path, configs: &[PathBuf]) -> PathBuf {
    let root = file.parent().unwrap();
    let compiled = argo_compiler::compile(file, configs, root).unwrap();
    assert!(compiled.full(), "{:?}", compiled.kept);
    let target = root.join("compiled.wir.json");
    std::fs::write(&target, serde_json::to_string(&compiled.document).unwrap()).unwrap();
    target
}

fn launch(target: &Path, output: &Path) -> Launch {
    Launch {
        target: target.to_owned(),
        scratch: output.with_extension("scratch"),
        output_dir: Some(output.to_owned()),
        ..Launch::default()
    }
}

/// Runs each task by writing its command into each of its outputs; tasks whose name holds
/// `fails` exit with 1, and the first task whose name holds `breaks` cannot run.
struct Writer {
    folder: PathBuf,
    fails: &'static str,
    breaks: &'static str,
    broke: bool,
}

impl Writer {
    fn new(folder: PathBuf) -> Writer {
        Writer {
            folder,
            fails: "\0",
            breaks: "\0",
            broke: false,
        }
    }
}

impl Executor for Writer {
    fn files(&self) -> Option<std::sync::Arc<dyn workflow_graph::Files>> {
        Some(std::sync::Arc::new(workflow_graph::LocalFiles))
    }

    fn run(&mut self, attempt: Attempt) -> Running {
        let task = &attempt.task;
        if task.name.contains(self.breaks) && !std::mem::replace(&mut self.broke, true) {
            let error = workflow_graph::ExecuteError::Machine {
                task: task.name.clone(),
                message: "the node went away".into(),
            };
            return Running::ended(Err(error));
        }
        std::fs::create_dir_all(&self.folder).unwrap();
        let files = (task.outputs.iter())
            .map(|output| {
                let name = output.trim_matches('/').replace('/', "-");
                let location = self
                    .folder
                    .join(format!("{}-{}-{name}", task.id, attempt.number));
                std::fs::write(&location, task.command.join(" ")).unwrap();
                OutputFile {
                    path: output.trim_start_matches('/').to_owned(),
                    location: location.to_string_lossy().into_owned(),
                    ..OutputFile::default()
                }
            })
            .collect();
        Running::ended(Ok(Outcome {
            id: task.id,
            attempt: attempt.number,
            exit_code: i32::from(task.name.contains(self.fails)),
            stdout: "out\n".into(),
            files,
            ..Outcome::default()
        }))
    }
}

/// How a run ended, and the names of its tasks, sorted.
fn ran(driver: &mut dyn Driver, executor: &mut dyn Executor) -> (Result<(), String>, Vec<String>) {
    let execution = execute("runs", driver, executor);
    let mut names: Vec<String> = (execution.graph.nodes.iter())
        .map(|node| node.task.name.clone())
        .collect();
    names.sort();
    let ended = execution
        .into_result()
        .map(|_| ())
        .map_err(|e| e.to_string());
    (ended, names)
}

/// The native and the compiled run of a workflow: how each ended and its task names.
type Both = (
    (Result<(), String>, Vec<String>),
    (Result<(), String>, Vec<String>),
);

fn both(root: &Path, file: &Path, configs: &[PathBuf], writer: fn(PathBuf) -> Writer) -> Both {
    let services = argo_runtime::Services::default();
    let mut alone = launch(file, &root.join("native"));
    alone.configs = configs.to_vec();
    let mut native = argo_runtime::load_driver(&alone, services.clone()).unwrap();
    let one = ran(&mut native, &mut writer(root.join("files")));
    let document = compiled(file, configs);
    let settings = wir_runtime::Settings {
        print: false,
        adapters: wir_runtime::argo::register(Default::default(), services),
        ..Default::default()
    };
    let wir = launch(&document, &root.join("wir"));
    let mut compiled = wir_runtime::WirDriver::new(&wir, settings).unwrap();
    let other = ran(&mut compiled, &mut writer(root.join("files")));
    (one, other)
}

/// The files below a folder, by relative path.
fn files(folder: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut open = vec![folder.to_owned()];
    while let Some(next) = open.pop() {
        for entry in std::fs::read_dir(&next).into_iter().flatten().flatten() {
            match entry.path().is_dir() {
                true => open.push(entry.path()),
                false => found.push(
                    entry
                        .path()
                        .strip_prefix(folder)
                        .unwrap()
                        .display()
                        .to_string(),
                ),
            }
        }
    }
    found.sort();
    found
}

const ARTIFACTS: &str = r#"  entrypoint: main
  templates:
  - name: main
    steps:
    - - {name: make, template: make}
    - - name: use
        template: use
        arguments: {artifacts: [{name: data, from: "{{steps.make.outputs.artifacts.data}}"}]}
  - name: make
    container: {image: alpine:3.20, command: [sh, -c, "echo hi > /tmp/data"]}
    outputs: {artifacts: [{name: data, path: /tmp/data, archive: {none: {}}}]}
  - name: use
    inputs: {artifacts: [{name: data, path: /in/data}]}
    container: {image: alpine:3.20, command: [cat, /in/data]}
"#;

#[test]
fn delivers_output_artifacts() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let file = workflow(root, ARTIFACTS);
    let ((one, _), (other, _)) = both(root, &file, &[], Writer::new);
    one.unwrap();
    other.unwrap();
    let native = files(&root.join("native"));
    assert!(!native.is_empty(), "the native run delivers its artifacts");
    assert_eq!(files(&root.join("wir")), native);
}

const BREAKS: &str = r#"  entrypoint: main
  templates:
  - name: main
    steps:
    - - {name: breaks, template: say}
    - - {name: after, template: say}
  - name: say
    retryStrategy: {limit: 1, retryPolicy: Always}
    container: {image: alpine:3.20, command: [echo, hi]}
"#;

#[test]
fn hands_errors_to_argo() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let file = workflow(root, BREAKS);
    let breaking = |folder| Writer {
        breaks: "breaks",
        ..Writer::new(folder)
    };
    let (one, other) = both(root, &file, &[], breaking);
    one.0.as_ref().unwrap();
    assert_eq!(other, one);
}

const LIBRARY: &str = r#"apiVersion: argoproj.io/v1alpha1
kind: WorkflowTemplate
metadata: {name: library}
spec:
  entrypoint: main
  templates:
  - name: main
    steps:
    - - {name: hello, template: say, arguments: {parameters: [{name: text, value: "{{workflow.name}}-x"}]}}
  - name: say
    inputs: {parameters: [{name: text}]}
    container: {image: alpine:3.20, command: [echo, "{{inputs.parameters.text}}"]}
"#;

#[test]
fn reads_templates_of_configs() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    // The config sits outside the folder of the workflow, whose templates Argo reads anyway.
    std::fs::create_dir_all(root.join("flow")).unwrap();
    let file = workflow(
        &root.join("flow"),
        "  workflowTemplateRef: {name: library}\n",
    );
    let configs = root.join("library.yaml");
    std::fs::write(&configs, LIBRARY).unwrap();
    let (one, other) = both(root, &file, &[configs], Writer::new);
    one.0.as_ref().unwrap();
    assert_eq!(one.1.len(), 1, "{one:?}");
    assert_eq!(other, one);
}

const EMPTY: &str = r#"  entrypoint: main
  templates:
  - name: main
    steps:
    - - {name: none, template: say, withItems: [], arguments: {parameters: [{name: text, value: "{{item}}"}]}}
    - - {name: after, template: say}
  - name: say
    inputs: {parameters: [{name: text, value: x}]}
    container: {image: alpine:3.20, command: [echo, "{{inputs.parameters.text}}"]}
"#;

#[test]
fn loops_over_no_items() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let file = workflow(root, EMPTY);
    let (one, other) = both(root, &file, &[], Writer::new);
    one.0.as_ref().unwrap();
    assert_eq!(one.1, ["runs[1].after"]);
    assert_eq!(other, one);
}

const SKIPPED: &str = r#"  entrypoint: main
  templates:
  - name: main
    steps:
    - - {name: produce, template: make, when: "1 == 2"}
    - - name: use
        template: say
        arguments: {parameters: [{name: text, value: "{{steps.produce.outputs.parameters.out}}"}]}
  - name: make
    container: {image: alpine:3.20, command: [sh, -c, "echo made > /tmp/out"]}
    outputs: {parameters: [{name: out, valueFrom: {path: /tmp/out, default: fallback}}]}
  - name: say
    inputs: {parameters: [{name: text}]}
    container: {image: alpine:3.20, command: [echo, "{{inputs.parameters.text}}"]}
"#;

#[test]
fn reads_defaults_of_skipped_steps() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let file = workflow(root, SKIPPED);
    let (one, other) = both(root, &file, &[], Writer::new);
    one.0.as_ref().unwrap();
    assert_eq!(one.1, ["runs[1].use"]);
    assert_eq!(other, one);
}
