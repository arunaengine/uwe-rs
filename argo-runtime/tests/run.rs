//! Runs small Argo workflows through the engine and the mock TES executor, and checks the
//! execution graphs: steps, DAGs, loops, conditions, outputs, artifacts, templates of other
//! documents, exit handlers, retries and hooks.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::rc::Rc;

use argo_runtime::{ArgoDriver, Library, Options, load_file};
use workflow_graph::{
    EdgeKind, ExecuteError, Execution, Executor, MockTes, Outcome, Task, TaskId, execute,
};

/// The mock executor, with standard output, exit codes and file contents chosen per template.
struct Scripted {
    mock: MockTes,
    /// Standard output of the calls of a template, in order; the last one repeats.
    stdout: HashMap<&'static str, Vec<&'static str>>,
    exit: HashMap<&'static str, Vec<i32>>,
    /// Contents of output files by their path, in the order tasks write them; the last one
    /// repeats.
    files: HashMap<&'static str, Vec<&'static str>>,
    calls: HashMap<String, usize>,
    writes: HashMap<String, usize>,
}

impl Executor for Scripted {
    fn execute(&mut self, task: &Task) -> Result<Outcome, ExecuteError> {
        let mut outcome = self.mock.execute(task)?;
        let call = self.calls.entry(task.step.clone()).or_default();
        fn pick<T: Clone>(list: Option<&Vec<T>>, call: usize) -> Option<T> {
            list.and_then(|list| list.get(call).or(list.last()).cloned())
        }
        if let Some(stdout) = pick(self.stdout.get(task.step.as_str()), *call) {
            outcome.stdout = format!("{stdout}\n");
        }
        if let Some(code) = pick(self.exit.get(task.step.as_str()), *call) {
            outcome.exit_code = code;
        }
        *call += 1;
        for file in &outcome.files {
            let path = format!("/{}", file.path.trim_start_matches('/'));
            let write = self.writes.entry(path.clone()).or_default();
            if let Some(content) = pick(self.files.get(path.as_str()), *write) {
                std::fs::write(&file.location, content).expect("the file is written");
                *write += 1;
            }
        }
        Ok(outcome)
    }
}

#[derive(Default)]
struct Setup {
    library: Vec<&'static str>,
    parameters: Vec<(&'static str, &'static str)>,
    stdout: Vec<(&'static str, Vec<&'static str>)>,
    exit: Vec<(&'static str, Vec<i32>)>,
    files: Vec<(&'static str, Vec<&'static str>)>,
}

struct Ran {
    execution: Execution,
    warnings: Vec<String>,
}

impl Ran {
    fn tasks(&self) -> Vec<&Task> {
        self.execution
            .graph
            .nodes
            .iter()
            .map(|node| &node.task)
            .collect()
    }

    /// Task names without the workflow name.
    fn names(&self) -> Vec<String> {
        self.tasks().iter().map(|task| short(&task.name)).collect()
    }

    fn task(&self, name: &str) -> &Task {
        self.tasks()
            .into_iter()
            .find(|task| short(&task.name) == name)
            .unwrap_or_else(|| panic!("no task {name} in {:?}", self.names()))
    }

    fn succeeded(&self) {
        if let Some(error) = &self.execution.error {
            panic!("the run fails: {error}");
        }
        self.execution.graph.check().expect("a valid graph");
    }

    fn error(&self) -> String {
        self.execution
            .error
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| panic!("the run succeeds with {:?}", self.names()))
    }

    /// The tasks a task waits for, by name.
    fn upstream(&self, name: &str) -> Vec<String> {
        let id = self.task(name).id;
        let name_of = |id: TaskId| {
            self.execution
                .graph
                .node(id)
                .map(|node| short(&node.task.name))
                .unwrap_or_default()
        };
        let mut names: Vec<String> = self
            .execution
            .graph
            .edges
            .iter()
            .filter(|edge| edge.to == id)
            .map(|edge| name_of(edge.from))
            .collect();
        names.sort();
        names
    }
}

fn short(name: &str) -> String {
    match name.find(['.', '[']) {
        Some(index) => name[index..].trim_start_matches('.').to_owned(),
        None => name.to_owned(),
    }
}

fn run_with(workflow: &str, setup: Setup) -> Ran {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("workflow.yaml");
    std::fs::write(&path, workflow).expect("the workflow is written");
    let documents = load_file(&path).expect("the workflow loads");
    let library = Library::new();
    library.add_documents(0, documents.iter().cloned());
    for (position, text) in setup.library.iter().enumerate() {
        let file = directory.path().join(format!("library-{position}.yaml"));
        std::fs::write(&file, text).expect("the library is written");
        library.add_documents(1, load_file(&file).expect("the library loads"));
    }
    let document = documents
        .iter()
        .find(|document| document.runnable())
        .cloned()
        .expect("a runnable document");
    let options = Options {
        parameters: setup
            .parameters
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
        scratch: directory.path().join("scratch"),
        now: Some("2026-09-28T12:00:00Z".parse().expect("a time")),
        ..Options::default()
    };
    let mut driver = ArgoDriver::new(document, Rc::new(library), options);
    let mut executor = Scripted {
        mock: MockTes::new(directory.path().join("mock")),
        stdout: setup.stdout.into_iter().collect(),
        exit: setup.exit.into_iter().collect(),
        files: setup.files.into_iter().collect(),
        calls: HashMap::new(),
        writes: HashMap::new(),
    };
    let execution = execute("workflow.yaml", &mut driver, &mut executor);
    let warnings = driver.warnings().to_vec();
    Ran {
        execution,
        warnings,
    }
}

fn run(workflow: &str) -> Ran {
    run_with(workflow, Setup::default())
}

const PRINT: &str = r#"
  - name: print
    inputs:
      parameters:
      - name: message
    container:
      image: busybox
      command: [echo]
      args: ["{{inputs.parameters.message}}"]
"#;

fn workflow(templates: &str) -> String {
    format!(
        "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata:\n  generateName: test-\nspec:\n  entrypoint: main\n  templates:\n{templates}{PRINT}"
    )
}

#[test]
fn steps_run_in_groups() {
    let ran = run(&workflow(
        r#"
  - name: main
    steps:
    - - name: hello1
        template: print
        arguments:
          parameters: [{name: message, value: "hello1 {{workflow.name}}"}]
    - - name: hello2a
        template: print
        arguments:
          parameters: [{name: message, value: hello2a}]
      - name: hello2b
        template: print
        arguments:
          parameters: [{name: message, value: hello2b}]
"#,
    ));
    ran.succeeded();
    assert_eq!(ran.names(), ["[0].hello1", "[1].hello2a", "[1].hello2b"]);
    let workflow_name = ran.tasks()[0]
        .name
        .split('[')
        .next()
        .expect("a name")
        .to_owned();
    assert!(workflow_name.starts_with("test-"));
    assert_eq!(
        ran.task("[0].hello1").command,
        ["echo", &format!("hello1 {workflow_name}")]
    );
    assert_eq!(ran.upstream("[1].hello2a"), ["[0].hello1"]);
    assert_eq!(ran.upstream("[1].hello2b"), ["[0].hello1"]);
    assert_eq!(ran.task("[1].hello2b").step, "print");
}

#[test]
fn loops_expand_items_params_and_sequences() {
    let ran = run_with(
        &workflow(
            r#"
  - name: main
    steps:
    - - name: gen
        template: print
        arguments:
          parameters: [{name: message, value: x}]
    - - name: items
        template: print
        arguments:
          parameters: [{name: message, value: "{{item.os}}-{{item.version}}"}]
        withItems:
        - {os: debian, version: 9}
        - {os: ubuntu, version: 24.04}
      - name: params
        template: print
        arguments:
          parameters: [{name: message, value: "{{item}}"}]
        withParam: "{{steps.gen.outputs.result}}"
      - name: sequence
        template: print
        arguments:
          parameters: [{name: message, value: "{{item}}"}]
        withSequence: {count: "2", format: "n%02d"}
    - - name: all
        template: print
        arguments:
          parameters: [{name: message, value: "{{steps.params.outputs.result}}"}]
"#,
        ),
        Setup {
            stdout: vec![(
                "print",
                vec!["[\"a\", 2]", "r1", "r2", "r3", "r4", "r5", "r6", "r7"],
            )],
            ..Setup::default()
        },
    );
    ran.succeeded();
    assert_eq!(
        ran.names(),
        [
            "[0].gen",
            "[1].items(0:os:debian,version:9)",
            "[1].items(1:os:ubuntu,version:24.04)",
            "[1].params(0:a)",
            "[1].params(1:2)",
            "[1].sequence(0:n00)",
            "[1].sequence(1:n01)",
            "[2].all"
        ]
    );
    assert_eq!(
        ran.task("[1].items(0:os:debian,version:9)").command,
        ["echo", "debian-9"]
    );
    // The results of the loop, in order, as a JSON list; texts that are not JSON stay texts.
    assert_eq!(ran.task("[2].all").command, ["echo", r#"["r3","r4"]"#]);
}

#[test]
fn when_decides_with_outputs() {
    let coinflip = r#"
  - name: main
    steps:
    - - name: flip
        template: print
        arguments:
          parameters: [{name: message, value: flip}]
    - - name: heads
        template: print
        arguments:
          parameters: [{name: message, value: heads}]
        when: "{{steps.flip.outputs.result}} == heads"
      - name: tails
        template: print
        arguments:
          parameters: [{name: message, value: tails}]
        when: "{{steps.flip.outputs.result}} == tails"
"#;
    let ran = run_with(
        &workflow(coinflip),
        Setup {
            stdout: vec![("print", vec!["heads"])],
            ..Setup::default()
        },
    );
    ran.succeeded();
    assert_eq!(ran.names(), ["[0].flip", "[1].heads"]);
    // Mock output is empty, which decides neither branch.
    let ran = run(&workflow(coinflip));
    ran.succeeded();
    assert_eq!(ran.names(), ["[0].flip"]);
}

#[test]
fn recursion_ends_by_outputs_and_depth_limit_fails_loudly() {
    let recursive = r#"
  - name: main
    steps:
    - - name: flip
        template: print
        arguments:
          parameters: [{name: message, value: flip}]
    - - name: again
        template: main
        when: "{{steps.flip.outputs.result}} == tails"
"#;
    let ran = run_with(
        &workflow(recursive),
        Setup {
            stdout: vec![("print", vec!["tails", "tails", "heads"])],
            ..Setup::default()
        },
    );
    ran.succeeded();
    assert_eq!(ran.tasks().len(), 3);
    let endless = r#"
  - name: main
    steps:
    - - name: again
        template: main
"#;
    // A hundred levels of templates need more stack than a test thread has in a debug build.
    let error = std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || run(&workflow(endless)).error())
        .expect("a thread")
        .join()
        .expect("the run ends");
    assert!(error.contains("levels deep"), "{error}");
}

#[test]
fn dag_depends_on_results() {
    let dag = r#"
  - name: main
    dag:
      tasks:
      - name: a
        template: print
        arguments: {parameters: [{name: message, value: a}]}
      - name: on-failure
        template: print
        depends: a.Failed
        arguments: {parameters: [{name: message, value: handle}]}
      - name: on-success
        template: print
        depends: a
        arguments: {parameters: [{name: message, value: next}]}
      - name: b
        template: print
        dependencies: [a]
        arguments: {parameters: [{name: message, value: b}]}
      - name: d
        template: print
        depends: "on-success && b"
        arguments: {parameters: [{name: message, value: d}]}
"#;
    let ran = run(&workflow(dag));
    ran.succeeded();
    assert_eq!(ran.names(), ["a", "on-success", "b", "d"]);
    assert_eq!(ran.upstream("d"), ["b", "on-success"]);
    let ran = run_with(
        &workflow(dag),
        Setup {
            exit: vec![("print", vec![1, 0])],
            ..Setup::default()
        },
    );
    // The handler runs, but the omitted tasks after `a` keep the failure of their branch.
    assert_eq!(ran.names(), ["a", "on-failure"]);
    assert!(
        ran.error().contains("Error (exit code 1)"),
        "{}",
        ran.error()
    );
}

#[test]
fn continue_on_and_retries() {
    let steps = r#"
  - name: main
    steps:
    - - name: flaky
        template: flaky
        continueOn: {failed: true}
    - - name: after
        template: print
        arguments: {parameters: [{name: message, value: "{{steps.flaky.status}}"}]}
  - name: flaky
    retryStrategy: {limit: 2}
    container:
      image: busybox
      command: [sh, -c, "exit $((RANDOM % 2))"]
"#;
    let ran = run_with(
        &workflow(steps),
        Setup {
            exit: vec![("flaky", vec![1, 1, 0])],
            ..Setup::default()
        },
    );
    ran.succeeded();
    assert_eq!(
        ran.names(),
        ["[0].flaky(0)", "[0].flaky(1)", "[0].flaky(2)", "[1].after"]
    );
    assert_eq!(ran.upstream("[0].flaky(1)"), ["[0].flaky(0)"]);
    assert_eq!(ran.task("[1].after").command, ["echo", "Succeeded"]);
    let ran = run_with(
        &workflow(steps),
        Setup {
            exit: vec![("flaky", vec![1])],
            ..Setup::default()
        },
    );
    ran.succeeded();
    assert_eq!(ran.task("[1].after").command, ["echo", "Failed"]);
}

#[test]
fn outputs_pass_parameters_and_artifacts() {
    let ran = run_with(
        &workflow(
            r#"
  - name: main
    dag:
      tasks:
      - name: make
        template: make
      - name: use
        template: use
        depends: make
        arguments:
          parameters:
          - name: count
            value: "{{tasks.make.outputs.parameters.count}}"
          - name: base
            value: "{{=sprig.osBase(tasks.make.outputs.parameters.file)}}"
          artifacts:
          - name: data
            from: "{{tasks.make.outputs.artifacts.data}}"
    outputs:
      parameters:
      - name: total
        valueFrom:
          expression: "asInt(tasks.make.outputs.parameters.count) * 2"
  - name: make
    script:
      image: python:3.13
      command: [python]
      source: |
        print("hi")
    outputs:
      parameters:
      - name: count
        valueFrom: {path: /tmp/count}
      - name: file
        valueFrom: {path: /tmp/file}
      artifacts:
      - name: data
        path: /tmp/data.txt
  - name: use
    inputs:
      parameters:
      - name: count
      - name: base
      artifacts:
      - name: data
        path: /in/data.txt
    container:
      image: busybox
      command: [cat, "{{inputs.artifacts.data.path}}", "{{inputs.parameters.count}}", "{{inputs.parameters.base}}"]
"#,
        ),
        Setup {
            files: vec![
                ("/tmp/count", vec!["21\n"]),
                ("/tmp/file", vec!["/data/run/x.bam"]),
            ],
            ..Setup::default()
        },
    );
    ran.succeeded();
    let make = ran.task("make");
    assert_eq!(make.command, ["python", "/argo/staging/script"]);
    assert_eq!(make.inputs[0].content.as_deref(), Some("print(\"hi\")\n"));
    let used = ran.task("use");
    assert_eq!(used.command, ["cat", "/in/data.txt", "21", "x.bam"]);
    assert_eq!(used.inputs[0].path, "/in/data.txt");
    let edge = ran
        .execution
        .graph
        .edges
        .iter()
        .find(|edge| edge.to == used.id)
        .expect("an edge into use");
    assert_eq!(
        edge.kind,
        EdgeKind::File {
            path: "/in/data.txt".to_owned()
        }
    );
}

#[test]
fn templates_of_other_documents() {
    let library = r#"
apiVersion: argoproj.io/v1alpha1
kind: WorkflowTemplate
metadata:
  name: lib
spec:
  entrypoint: whalesay
  arguments:
    parameters: [{name: greeting, value: from-template}]
  templates:
  - name: whalesay
    inputs:
      parameters: [{name: message, value: "{{workflow.parameters.greeting}}"}]
    steps:
    - - name: inner
        template: say
        arguments: {parameters: [{name: message, value: "{{inputs.parameters.message}}"}]}
  - name: say
    inputs:
      parameters: [{name: message}]
    container:
      image: docker/whalesay
      command: [cowsay, "{{inputs.parameters.message}}"]
"#;
    let referencing = r#"
apiVersion: argoproj.io/v1alpha1
kind: Workflow
metadata:
  generateName: ref-
spec:
  entrypoint: main
  arguments:
    parameters: [{name: greeting, value: hello}]
  templates:
  - name: main
    steps:
    - - name: call
        templateRef: {name: lib, template: whalesay}
"#;
    let ran = run_with(
        referencing,
        Setup {
            library: vec![library],
            ..Setup::default()
        },
    );
    ran.succeeded();
    assert_eq!(ran.task("[0].call[0].inner").command, ["cowsay", "hello"]);
    let submitted = r#"
apiVersion: argoproj.io/v1alpha1
kind: Workflow
metadata:
  generateName: submitted-
spec:
  workflowTemplateRef: {name: lib}
"#;
    let ran = run_with(
        submitted,
        Setup {
            library: vec![library],
            parameters: vec![("greeting", "from-cli")],
            ..Setup::default()
        },
    );
    ran.succeeded();
    assert_eq!(ran.task("[0].inner").command, ["cowsay", "from-cli"]);
    let missing = run(referencing);
    assert!(missing.error().contains("WorkflowTemplate lib not found"));
}

#[test]
fn exit_handlers_see_the_status() {
    let ran = run_with(
        &format!(
            "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata:\n  name: exits\nspec:\n  entrypoint: main\n  onExit: exit\n  templates:\n{}{PRINT}",
            r#"
  - name: main
    steps:
    - - name: work
        template: print
        arguments: {parameters: [{name: message, value: work}]}
        hooks:
          exit:
            template: print
            arguments: {parameters: [{name: message, value: "cleanup {{steps.work.status}}"}]}
  - name: exit
    steps:
    - - name: report
        template: print
        arguments: {parameters: [{name: message, value: "{{workflow.status}} {{workflow.failures}}"}]}
"#
        ),
        Setup {
            exit: vec![("print", vec![1, 0])],
            ..Setup::default()
        },
    );
    assert_eq!(
        ran.names(),
        ["[0].work", "[0].work.onExit", "onExit[0].report"]
    );
    assert_eq!(
        ran.task("[0].work.onExit").command,
        ["echo", "cleanup Failed"]
    );
    // Argo quotes the JSON of every failed node, the steps and their group too.
    let report = &ran.task("onExit[0].report").command[1];
    assert!(
        report.starts_with(r#"Failed "[{\"displayName\":\"exits\""#),
        "{report}"
    );
    assert!(
        report.contains(r#"{\"displayName\":\"work\",\"message\":\"Error (exit code 1)\","#),
        "{report}"
    );
    assert_eq!(ran.upstream("onExit[0].report"), ["[0].work.onExit"]);
    assert!(ran.error().contains("workflow failed"));
}

#[test]
fn generated_names_do_not_depend_on_the_location() {
    // Each run writes the workflow into a new temporary directory.
    let names: Vec<String> = (0..2)
        .map(|_| run(&workflow(PRINT_STEP)).tasks()[0].name.clone())
        .collect();
    assert!(names[0].starts_with("test-"), "{names:?}");
    assert_eq!(names[0], names[1]);
}

const PRINT_STEP: &str = r#"
  - name: main
    steps:
    - - name: hello
        template: print
        arguments: {parameters: [{name: message, value: hello}]}
"#;

#[test]
fn placeholders_for_missing_parameters() {
    let ran = run(&workflow(
        r#"
  - name: main
    inputs:
      parameters: [{name: sample}, {name: kind, enum: [fastq, bam]}]
    steps:
    - - name: show
        template: print
        arguments: {parameters: [{name: message, value: "{{inputs.parameters.sample}} {{inputs.parameters.kind}}"}]}
"#,
    ));
    ran.succeeded();
    assert_eq!(
        ran.task("[0].show").command,
        ["echo", "placeholder_sample fastq"]
    );
    assert!(
        ran.warnings
            .iter()
            .any(|warning| warning.contains("placeholder_sample"))
    );
}

#[test]
fn leaf_kinds_become_tasks() {
    let ran = run(&workflow(
        r#"
  - name: main
    steps:
    - - name: set
        template: set
      - name: resource
        template: resource
      - name: http
        template: http
      - name: wait
        template: wait
  - name: set
    containerSet:
      containers:
      - name: first
        image: alpine
        command: [sh, -c, "echo a > /mnt/a"]
        env:
        - {name: POD, valueFrom: {fieldRef: {fieldPath: metadata.name}}}
        - {name: TOKEN, valueFrom: {secretKeyRef: {name: s, key: k}}}
      - {name: main, image: alpine, command: [cat, /mnt/a], dependencies: [first]}
  - name: resource
    resource:
      action: create
      manifest: |
        apiVersion: v1
        kind: ConfigMap
  - name: http
    http:
      url: https://example.org/{{workflow.name}}
      method: POST
      body: data
  - name: wait
    suspend: {duration: 10s}
"#,
    ));
    ran.succeeded();
    assert_eq!(
        ran.names(),
        [
            "[0].set (first)",
            "[0].set (main)",
            "[0].resource",
            "[0].http"
        ]
    );
    assert_eq!(ran.upstream("[0].set (main)"), ["[0].set (first)"]);
    // Pods get made-up fields, while Secrets are left out.
    let environment = &ran.task("[0].set (first)").environment;
    assert!(environment["POD"].starts_with("test-"), "{environment:?}");
    assert!(!environment.contains_key("TOKEN"));
    assert_eq!(
        ran.task("[0].resource").command[..2],
        ["argoexec", "resource"]
    );
    let http = &ran.task("[0].http").command;
    assert_eq!(http[..2], ["sh", "-c"]);
    assert!(http[2].starts_with("curl --silent"), "{}", http[2]);
    assert!(
        http[2].contains(" https://example.org/test-"),
        "{}",
        http[2]
    );
    assert!(http[2].ends_with("> /tmp/response-status"), "{}", http[2]);
    assert!(
        ran.warnings
            .iter()
            .any(|warning| warning.contains("response status is not known"))
    );
}

/// A workflow with a plugin template and a step that prints its output; `plugins` is the text of
/// `spec.executorPlugins`.
fn plugin_workflow(plugins: &str) -> String {
    format!(
        r#"apiVersion: argoproj.io/v1alpha1
kind: Workflow
metadata:
  generateName: test-
spec:
  entrypoint: main
{plugins}  templates:
  - name: main
    steps:
    - - name: ask
        template: ask
    - - name: show
        template: print
        arguments: {{parameters: [{{name: message, value: "{{{{steps.ask.outputs.parameters.greeting}}}}"}}]}}
  - name: ask
    plugin:
      hello: {{name: world}}
    outputs:
      parameters: [{{name: greeting, valueFrom: {{default: unknown}}}}]
{PRINT}"#
    )
}

#[test]
fn plugin_templates_call_executor_plugins() {
    // Workflow-level plugins; a mock run does not know the reply.
    let plugins = "  executorPlugins:\n  - metadata: {name: hello}\n    spec:\n      sidecar:\n        container: {name: hello, image: python:alpine, command: [python, -c], args: [serve], ports: [{containerPort: 4356}]}\n";
    let ran = run(&plugin_workflow(plugins));
    ran.succeeded();
    let task = ran.task("[0].ask");
    assert_eq!(task.image.as_deref(), Some("python:alpine"));
    assert_eq!(task.command[..2], ["sh", "-c"]);
    assert_eq!(
        task.command[3..],
        ["call-plugin", "4356", "python", "-c", "serve"]
    );
    let request = task
        .inputs
        .iter()
        .find(|input| input.path == "/argo/plugin/request.json")
        .and_then(|input| input.content.clone())
        .expect("a request");
    assert!(
        request.starts_with(r#"{"workflow":{"metadata":{"name":"test-"#),
        "{request}"
    );
    assert!(
        request.contains(r#""plugin":{"hello":{"name":"world"}}"#),
        "{request}"
    );
    assert_eq!(ran.task("[1].show").command, ["echo", "unknown"]);
    assert!(
        ran.warnings
            .iter()
            .any(|warning| warning.contains("reply is not known"))
    );

    // Without plugins no one runs the template, as in Argo.
    let ran = run(&plugin_workflow(""));
    assert!(
        ran.error().contains("no plugin executed the template"),
        "{}",
        ran.error()
    );
}
