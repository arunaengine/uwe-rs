//! Checks Argo execution graphs with small workflows and the simulator.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::rc::Rc;

use argo_runtime::{ArgoDriver, Library, Options, load_file};
use workflow_graph::{
    Attempt, EdgeKind, ExecuteError, Execution, Executor, Outcome, Running, Simulator, Task,
    TaskId, execute,
};

/// The simulator, with standard output, exit codes and file contents chosen per template.
struct Scripted {
    simulator: Simulator,
    /// Standard output of the calls of a template, in order; the last one repeats.
    stdout: HashMap<&'static str, Vec<&'static str>>,
    exit: HashMap<&'static str, Vec<i32>>,
    /// Contents of output files by their path, in the order tasks write them; the last one
    /// repeats.
    files: HashMap<&'static str, Vec<&'static str>>,
    calls: HashMap<String, usize>,
    writes: HashMap<String, usize>,
    /// Exit codes of containers of tasks that run several, by template and container; the
    /// others end with 0.
    containers: HashMap<&'static str, Vec<(&'static str, i32)>>,
    /// Directory outputs hold the files their task got below them, as container mounts do.
    mounts: bool,
    /// The names in the input folders of each task when it ran, by task.
    listings: HashMap<String, Vec<String>>,
}

impl Executor for Scripted {
    fn run(&mut self, attempt: Attempt) -> Running {
        Running::ended(self.execute(&attempt.task))
    }
}

impl Scripted {
    fn execute(&mut self, task: &Task) -> Result<Outcome, ExecuteError> {
        let mut outcome = self.simulator.simulate(task)?;
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
        if let Some(codes) = self.containers.get(task.step.as_str()) {
            let code = |name: &str| {
                (codes.iter())
                    .find(|(known, _)| *known == name)
                    .map_or(0, |(_, code)| *code)
            };
            outcome.containers = (task.containers.iter())
                .map(|container| workflow_graph::ContainerOutcome {
                    name: container.name.clone(),
                    exit_code: Some(code(&container.name)),
                    ..Default::default()
                })
                .collect();
            let codes: Vec<i32> = outcome
                .containers
                .iter()
                .filter_map(|c| c.exit_code)
                .collect();
            outcome.exit_code = match codes.first() {
                Some(&main) if main != 0 => main,
                _ => codes.into_iter().find(|code| *code != 0).unwrap_or(0),
            };
        }
        if self.mounts {
            let mut names = Vec::new();
            for input in task.inputs.iter().filter(|input| input.directory) {
                for entry in std::fs::read_dir(&input.source).into_iter().flatten() {
                    let name = entry.expect("an entry").file_name();
                    names.push(format!("{}/{}", input.path, name.to_string_lossy()));
                }
            }
            names.sort();
            self.listings.insert(short(&task.name), names);
            keep_given(task, &outcome);
        }
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

/// Copies the input folder of each directory output into it, as a container mount, except
/// the files that `rm` names; `touch` makes a file.
fn keep_given(task: &Task, outcome: &Outcome) {
    let words = task.command.join(" ");
    for directory in outcome.files.iter().filter(|file| file.directory) {
        let mount = format!("/{}", directory.path.trim_matches('/'));
        let given = task.inputs.iter().find(|input| input.path == mount);
        let Some(source) = given.map(|input| std::path::Path::new(&input.source)) else {
            continue;
        };
        let location = std::path::Path::new(&directory.location);
        for entry in std::fs::read_dir(source).expect("the input folder") {
            let entry = entry.expect("an entry");
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let target = location.join(&*name);
            if entry.file_type().expect("a type").is_symlink() {
                let link = std::fs::read_link(entry.path()).expect("a link");
                std::os::unix::fs::symlink(link, target).expect("the link is copied");
            } else if !words.contains(&format!("rm {mount}/{name}")) {
                std::fs::copy(source.join(&*name), target).expect("the input is copied");
            }
        }
        let prefix = format!("touch {mount}/");
        for made in (words.split(&prefix).skip(1)).filter_map(|rest| rest.split_whitespace().next())
        {
            std::fs::write(location.join(made), "made").expect("the file is made");
        }
    }
}

#[derive(Default)]
struct Setup {
    library: Vec<&'static str>,
    parameters: Vec<(&'static str, &'static str)>,
    stdout: Vec<(&'static str, Vec<&'static str>)>,
    exit: Vec<(&'static str, Vec<i32>)>,
    files: Vec<(&'static str, Vec<&'static str>)>,
    /// Run as a real run, which gives no placeholders for missing inputs.
    real_inputs: bool,
    /// Go on after a failure, as `-keep-going` asks.
    keep_going: bool,
    containers: Vec<(&'static str, Vec<(&'static str, i32)>)>,
    mounts: bool,
    /// The folder of `iunctus resume` requests.
    resume: Option<std::path::PathBuf>,
}

struct Ran {
    execution: Execution,
    warnings: Vec<String>,
    listings: HashMap<String, Vec<String>>,
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

    /// The reasons of the markers of a task.
    fn markers(&self, name: &str) -> Vec<&str> {
        let node = (self.execution.graph.nodes.iter())
            .find(|node| short(&node.task.name) == name)
            .unwrap_or_else(|| panic!("no task {name} in {:?}", self.names()));
        node.markers
            .iter()
            .map(|marker| marker.reason.as_str())
            .collect()
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
    let mut driver = build(directory.path(), workflow, &setup);
    let mut executor = Scripted {
        simulator: Simulator::new(directory.path().join("simulator")),
        stdout: setup.stdout.into_iter().collect(),
        exit: setup.exit.into_iter().collect(),
        files: setup.files.into_iter().collect(),
        calls: HashMap::new(),
        writes: HashMap::new(),
        containers: setup.containers.into_iter().collect(),
        mounts: setup.mounts,
        listings: HashMap::new(),
    };
    let execution = execute("workflow.yaml", &mut driver, &mut executor);
    let warnings = driver.warnings().to_vec();
    Ran {
        execution,
        warnings,
        listings: executor.listings,
    }
}

/// The driver of a workflow with the choices of a setup, in a folder of the test.
fn build(directory: &std::path::Path, workflow: &str, setup: &Setup) -> ArgoDriver {
    let path = directory.join("workflow.yaml");
    std::fs::write(&path, workflow).expect("the workflow is written");
    let documents = load_file(&path).expect("the workflow loads");
    let library = Library::new();
    library.add_documents(0, documents.iter().cloned());
    for (position, text) in setup.library.iter().enumerate() {
        let file = directory.join(format!("library-{position}.yaml"));
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
        scratch: directory.join("scratch"),
        now: Some("2026-09-28T12:00:00Z".parse().expect("a time")),
        assume_inputs: !setup.real_inputs,
        keep_going: setup.keep_going,
        resume: setup.resume.clone(),
        ..Options::default()
    };
    ArgoDriver::new(document, Rc::new(library), options)
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
fn groups_workflow_steps() {
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
fn expands_argo_loops() {
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
    // The loop over the output of `gen` decides how many tasks follow it; the argument of
    // `all` decides none.
    assert_eq!(
        ran.markers("[0].gen"),
        ["the `withParam` of step `params` reads the outputs of this task"]
    );
    assert_eq!(ran.execution.graph.marked().count(), 1);
}

#[test]
fn conditions_read_outputs() {
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
    // Simulated output is empty, which decides neither branch; a real run takes one.
    let ran = run(&workflow(coinflip));
    ran.succeeded();
    assert_eq!(ran.names(), ["[0].flip"]);
    assert_eq!(
        ran.markers("[0].flip"),
        [
            "the `when` of step `heads` reads the outputs of this task",
            "the `when` of step `tails` reads the outputs of this task"
        ]
    );
}

#[test]
fn bounds_recursive_calls() {
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
fn dag_requires_results() {
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
        depends: a
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
fn failure_preserves_branches() {
    let dag = r#"
  - name: fail
    container: {image: busybox, command: ["false"]}
  - name: main
    dag:
      tasks:
      - name: a
        template: fail
      - name: b
        template: print
        arguments: {parameters: [{name: message, value: b}]}
      - name: c
        template: print
        depends: b
        arguments: {parameters: [{name: message, value: c}]}
"#;
    let failing = |keep_going| Setup {
        exit: vec![("fail", vec![1])],
        keep_going,
        ..Setup::default()
    };
    // Argo admits ready dependencies before assessing DAG failure; `-keep-going` disables
    // default fail-fast.
    for keep_going in [false, true] {
        let ran = run_with(&workflow(dag), failing(keep_going));
        assert_eq!(ran.names(), ["a", "b", "c"]);
        assert!(
            ran.error().contains("Error (exit code 1)"),
            "{}",
            ran.error()
        );
    }
}

#[test]
fn removes_host_files() {
    let host = tempfile::tempdir().expect("a host folder");
    for name in ["old", "keep"] {
        std::fs::write(host.path().join(name), name).expect("writes a host file");
    }
    let text = format!(
        r#"
  - name: main
    volumes: [{{name: data, hostPath: {{path: {}, type: Directory}}}}]
    container: {{image: alpine, command: [sh, -c, "rm /data/old"], volumeMounts: [{{name: data, mountPath: /data}}]}}
"#,
        host.path().display()
    );
    let mounts = Setup {
        mounts: true,
        ..real()
    };
    run_with(&workflow(&text), mounts).succeeded();
    // A real host path loses the file that the task removed, as in Argo.
    assert!(!host.path().join("old").exists());
    assert_eq!(
        std::fs::read_to_string(host.path().join("keep")).unwrap(),
        "keep"
    );
}

#[test]
fn siblings_keep_changes() {
    use std::os::unix::fs::PermissionsExt;
    let host = tempfile::tempdir().expect("a host folder");
    for name in ["old", "keep"] {
        std::fs::write(host.path().join(name), name).expect("writes a host file");
    }
    let fixed = host.path().join("keep");
    std::fs::set_permissions(&fixed, std::fs::Permissions::from_mode(0o444)).expect("mode");
    let text = format!(
        r#"
  - name: main
    steps:
    - - {{name: remove, template: work, arguments: {{parameters: [{{name: do, value: rm /data/old}}]}}}}
      - {{name: make, template: work, arguments: {{parameters: [{{name: do, value: touch /data/new}}]}}}}
  - name: work
    inputs: {{parameters: [{{name: do}}]}}
    volumes: [{{name: data, hostPath: {{path: {}, type: Directory}}}}]
    container: {{image: alpine, command: [sh, -c, "{{{{inputs.parameters.do}}}}"], volumeMounts: [{{name: data, mountPath: /data}}]}}
"#,
        host.path().display()
    );
    let mounts = Setup {
        mounts: true,
        ..real()
    };
    run_with(&workflow(&text), mounts).succeeded();
    // Both changes stay, as on one shared host path; the read-only file is not written.
    assert!(
        !host.path().join("old").exists(),
        "the stale copy came back"
    );
    assert_eq!(
        std::fs::read_to_string(host.path().join("new")).unwrap(),
        "made"
    );
    assert_eq!(std::fs::read_to_string(&fixed).unwrap(), "keep");
    std::fs::set_permissions(&fixed, std::fs::Permissions::from_mode(0o644)).expect("mode");
}

#[test]
fn host_links_stay() {
    let host = tempfile::tempdir().expect("a host folder");
    let folder = host.path().join("data");
    std::fs::create_dir(&folder).expect("a folder");
    std::fs::write(folder.join("a"), "a").expect("writes a host file");
    for name in ["self", "again"] {
        std::os::unix::fs::symlink(".", folder.join(name)).expect("links");
    }
    let text = format!(
        r#"
  - name: main
    volumes: [{{name: data, hostPath: {{path: {}, type: Directory}}}}]
    container: {{image: alpine, command: [sh, -c, "touch /data/b"], volumeMounts: [{{name: data, mountPath: /data}}]}}
"#,
        folder.display()
    );
    let mounts = || Setup {
        mounts: true,
        ..real()
    };
    run_with(&workflow(&text), mounts()).succeeded();
    assert!(folder.join("self").is_symlink());
    assert_eq!(std::fs::read_to_string(folder.join("b")).unwrap(), "made");
    // A link that leads out of the host path is refused, not read.
    std::os::unix::fs::symlink("../secret", folder.join("out")).expect("links");
    let ran = run_with(&workflow(&text), mounts());
    assert!(ran.error().contains("leads out of it"), "{}", ran.error());
}

#[test]
fn runs_sidecars_beside() {
    let text = r#"
  - name: main
    container: {image: alpine, command: [wget], args: ["http://127.0.0.1:8080/"]}
    sidecars:
    - {name: web, image: python, command: [python3, -m, http.server, "8080"], volumeMounts: [{name: data, mountPath: /data}]}
    volumes: [{name: data, emptyDir: {}}]
"#;
    let ran = run(&workflow(text));
    ran.succeeded();
    let task = ran.tasks()[0];
    let names: Vec<(&str, bool)> = (task.containers.iter())
        .map(|container| (container.name.as_str(), container.service))
        .collect();
    assert_eq!(names, [("main", false), ("web", true)]);
    assert_eq!(
        task.containers[0].command,
        ["wget", "http://127.0.0.1:8080/"]
    );
    assert_eq!(task.containers[1].image.as_deref(), Some("python"));
    // The volumes of the sidecar are the task's too.
    assert!(
        task.inputs.iter().any(|input| input.path == "/data"),
        "{:?}",
        task.inputs
    );
}

#[test]
fn resumes_suspended_steps() {
    let text = r#"
  - name: main
    steps:
    - - {name: approve, template: approve}
    - - name: show
        template: print
        arguments: {parameters: [{name: message, value: "{{steps.approve.outputs.parameters.message}}"}]}
  - name: approve
    suspend: {}
    outputs: {parameters: [{name: message, valueFrom: {supplied: {}}}]}
"#;
    // Without a way to hear `iunctus resume`, a real run cannot wait.
    assert!(
        run_with(&workflow(text), real())
            .error()
            .contains("iunctus resume")
    );
    let folder = tempfile::tempdir().expect("a folder");
    let request = argo_runtime::resume::Request {
        step: Some("approve".into()),
        outputs: [("message".to_owned(), "yes".to_owned())].into(),
    };
    request.write(folder.path()).expect("writes the request");
    let resumable = Setup {
        resume: Some(folder.path().to_owned()),
        ..real()
    };
    let ran = run_with(&workflow(text), resumable);
    ran.succeeded();
    assert_eq!(ran.task("[1].show").command, ["echo", "yes"]);
}

#[test]
fn resumes_own_scope() {
    use workflow_graph::Driver;
    let text = r#"
  - name: main
    steps:
    - - {name: approve, template: approve}
      - {name: start, template: start}
    - - {name: show, template: print, arguments: {parameters: [{name: message, value: done}]}}
  - name: approve
    suspend: {}
  - name: start
    resource:
      action: create
      manifest: |
        apiVersion: argoproj.io/v1alpha1
        kind: Workflow
        metadata: {name: kid}
        spec:
          entrypoint: main
          templates:
          - name: main
            steps:
            - - {name: approve, template: hold}
            - - {name: after, template: after}
          - name: hold
            suspend: {}
          - name: after
            container: {image: alpine, command: [echo, after]}
"#;
    let directory = tempfile::tempdir().expect("a folder");
    let requests = directory.path().join("requests");
    let setup = Setup {
        resume: Some(requests.clone()),
        ..real()
    };
    let mut driver = build(directory.path(), &workflow(text), &setup);
    assert!(driver.start().expect("starts").is_empty());
    let mut wake = |step: Option<&str>| -> Vec<String> {
        let request = argo_runtime::resume::Request {
            step: step.map(str::to_owned),
            ..Default::default()
        };
        request.write(&requests).expect("writes the request");
        let tasks = driver.wake().expect("wakes");
        tasks.iter().map(|task| short(&task.name)).collect()
    };
    // The child does not take the request of its parent, nor the reverse.
    assert_eq!(wake(Some("approve")), ["[1].show"]);
    assert_eq!(wake(Some("kid")), ["[1].after"]);
}

#[test]
fn stops_after_failure() {
    let steps = r#"
  - name: main
    failFast: true
    parallelism: 1
    steps:
    - - name: each
        template: print
        arguments: {parameters: [{name: message, value: "{{item}}"}]}
        withItems: [a, b, c]
"#;
    let failing = Setup {
        exit: vec![("print", vec![1, 0])],
        ..Setup::default()
    };
    let ran = run_with(&workflow(steps), failing);
    // Argo starts no more items once one failed, and fails the template.
    assert_eq!(ran.names(), ["[0].each(0:a)"]);
    let error = ran.error();
    assert!(error.contains("each(0:a): Error (exit code 1)"), "{error}");
}

#[test]
fn retries_continue_failures() {
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
fn passes_template_outputs() {
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
fn loads_external_templates() {
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
    assert_eq!(
        run(submitted).error(),
        "couldn't find workflow template \"lib\""
    );
    let missing = run(referencing);
    assert!(
        missing
            .error()
            .contains("couldn't find workflow template \"lib\""),
        "{}",
        missing.error()
    );
}

#[test]
fn handlers_read_status() {
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
fn preserves_retry_results() {
    for spec in [
        "  retryStrategy: {limit: 1, backoff: {duration: 5s, factor: 2, cap: 1m}, affinity: {nodeAntiAffinity: {}}}\n  templates:\n  - name: main\n    container: {image: alpine}\n",
        "  templates:\n  - name: main\n    retryStrategy: {limit: 1, backoff: {duration: 5s, factor: 2, cap: 1m}, affinity: {nodeAntiAffinity: {}}}\n    container: {image: alpine}\n",
    ] {
        let ran = run_with(
            &format!(
                "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {{name: retries}}\nspec:\n  entrypoint: main\n{spec}"
            ),
            Setup {
                exit: vec![("main", vec![1, 0])],
                ..Setup::default()
            },
        );
        ran.succeeded();
        assert_eq!(ran.tasks().len(), 2);
    }
}

#[test]
fn rejects_invalid_hooks() {
    let retried = run_with(
        &workflow(
            "  - name: main\n    retryStrategy: {limit: 1, expression: 'missing.name'}\n    container: {image: alpine}\n",
        ),
        Setup {
            exit: vec![("main", vec![1])],
            ..Setup::default()
        },
    );
    assert!(
        retried.error().contains("retryStrategy.expression"),
        "{}",
        retried.error()
    );
    let workflow_hook = run(
        "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {name: hook}\nspec:\n  entrypoint: main\n  hooks:\n    check: {template: main, expression: 'missing.name'}\n  templates:\n  - name: main\n    container: {image: alpine}\n",
    );
    assert!(
        workflow_hook.error().contains("hook check"),
        "{}",
        workflow_hook.error()
    );
    let step_hook = run(&workflow(
        "  - name: main\n    steps:\n    - - name: use\n        template: work\n        hooks:\n          check: {template: work, expression: 'missing.name'}\n  - name: work\n    container: {image: alpine}\n",
    ));
    assert!(
        step_hook.error().contains("hooks.check"),
        "{}",
        step_hook.error()
    );
}

#[test]
fn resolves_retry_limits() {
    let ran = run_with(
        "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {name: retries}\nspec:\n  entrypoint: main\n  arguments: {parameters: [{name: limit, value: '0'}]}\n  retryStrategy: {limit: '{{workflow.parameters.limit}}'}\n  templates:\n  - name: main\n    container: {image: alpine}\n",
        Setup {
            exit: vec![("main", vec![1, 0])],
            ..Setup::default()
        },
    );
    assert_eq!(ran.tasks().len(), 1);
    assert!(ran.error().contains("failed"));
}

#[test]
fn names_ignore_location() {
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
fn supplies_missing_placeholders() {
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
    // A real run needs the values, as `argo submit` does.
    let real = Setup {
        real_inputs: true,
        ..Setup::default()
    };
    let ran = run_with(
        &workflow(
            "  - name: main\n    inputs: {parameters: [{name: sample}]}\n    container: {image: busybox}\n",
        ),
        real,
    );
    assert_eq!(
        ran.error(),
        "spec.arguments.sample.value or spec.arguments.sample.valueFrom is required"
    );
    // A value that its enum does not allow stops the run before it starts, as in Argo.
    let ran = run_with(
        "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {generateName: test-}\nspec:\n  entrypoint: main\n  arguments:\n    parameters: [{name: kind, enum: [fastq, bam]}]\n  templates:\n  - name: main\n    container: {image: busybox}\n",
        Setup {
            parameters: vec![("kind", "cram")],
            ..Setup::default()
        },
    );
    assert_eq!(
        ran.error(),
        "spec.arguments.kind.value should be present in spec.arguments.kind.enum list"
    );
    // So do templates that are not there or have no type.
    let ran = run(&workflow(
        "  - name: main\n    container: {image: busybox}\n  - name: typeless\n    inputs: {parameters: [{name: x}]}\n",
    ));
    assert_eq!(
        ran.error(),
        "templates.typeless template 'typeless' type is unknown"
    );
    let ran = run(&workflow(
        "  - name: other\n    container: {image: busybox}\n",
    ));
    assert_eq!(ran.error(), "template name 'main' undefined");
}

#[test]
fn memoization_preserves_execution() {
    for spec in [
        "  templates:\n  - name: main\n    memoize: {key: cache, cache: {configMap: {name: cache}}}\n    container: {image: alpine}\n",
        "  templateDefaults:\n    memoize: {key: cache, cache: {configMap: {name: cache}}}\n  templates:\n  - name: main\n    container: {image: alpine}\n",
    ] {
        let ran = run(&format!(
            "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {{name: memoized}}\nspec:\n  entrypoint: main\n{spec}",
        ));
        ran.succeeded();
        assert_eq!(ran.tasks().len(), 1);
        assert!(
            ran.warnings
                .iter()
                .any(|warning| warning.contains("native content-addressed cache"))
        );
    }
}

#[test]
fn limits_override_requests() {
    for (resources, cpus, memory) in [
        (
            "requests: {cpu: 500m, memory: 1Gi}, limits: {cpu: 2, memory: 2Gi}",
            2.0,
            2 << 30,
        ),
        ("requests: {cpu: 500m, memory: 1Gi}", 0.5, 1 << 30),
        ("limits: {cpu: 2, memory: 2Gi}", 2.0, 2 << 30),
        ("requests: {cpu: 500m}, limits: {memory: 2Gi}", 0.5, 2 << 30),
    ] {
        let ran = run(&workflow(&format!(
            "  - name: main\n    container: {{image: alpine, resources: {{{resources}}}}}\n",
        )));
        ran.succeeded();
        assert_eq!(ran.tasks()[0].resources.cpus, Some(cpus));
        assert_eq!(ran.tasks()[0].resources.memory_bytes, Some(memory));
    }
}

#[test]
fn lowers_leaf_tasks() {
    let ran = run(&workflow(
        r#"
  - name: main
    steps:
    - - name: set
        template: set
      - name: http
        template: http
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
  - name: http
    http:
      url: https://example.org/{{workflow.name}}
      method: POST
      body: data
"#,
    ));
    ran.succeeded();
    assert_eq!(ran.names(), ["[0].set", "[0].http"]);
    let set = ran.task("[0].set");
    let names: Vec<&str> = set.containers.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["main", "first"]);
    assert_eq!(set.containers[0].after, ["first"]);
    // Pods get made-up fields, and Secrets placeholders in a simulated run.
    let environment = &set.containers[1].environment;
    assert!(environment["POD"].starts_with("test-"), "{environment:?}");
    assert_eq!(environment["TOKEN"], "placeholder_secret_s_k");
    let http = &ran.task("[0].http").command;
    assert_eq!(http[..2], ["sh", "-c"]);
    let script = &http[2];
    assert!(script.starts_with("printf '%s' data | curl --silent"));
    assert!(script.contains("--data-binary @-"));
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

const CONTAINER_SET: &str = r#"
  - name: main
    dag:
      tasks:
      - {name: set, template: set}
      - name: after
        template: print
        dependencies: [set]
        arguments: {parameters: [{name: message, value: "{{tasks.set.outputs.parameters.message}}"}]}
  - name: set
    volumes: [{name: workspace, emptyDir: {}}]
    containerSet:
      retryStrategy: {retries: "3", duration: 5s}
      volumeMounts: [{name: workspace, mountPath: /workspace}]
      containers:
      - name: a
        image: alpine
        command: [sh, -c, "echo a > /workspace/message"]
        resources: {requests: {cpu: 500m, memory: 1Gi}}
      - {name: b, image: busybox, command: [sh, -c, "echo b >> /workspace/message"], dependencies: [a], workingDir: /tmp}
      - name: main
        image: alpine
        command: [cat, /workspace/message]
        dependencies: [b]
        resources: {requests: {cpu: "1", memory: 1Gi}}
    outputs:
      parameters: [{name: message, valueFrom: {path: /workspace/message}}]
"#;

#[test]
fn groups_container_set() {
    let setup = Setup {
        files: vec![("/workspace/message", vec!["a\nb\n"])],
        ..Setup::default()
    };
    let ran = run_with(&workflow(CONTAINER_SET), setup);
    ran.succeeded();
    assert_eq!(ran.names(), ["set", "after"]);
    let set = ran.task("set");
    assert_eq!(set.image.as_deref(), Some("alpine"));
    assert_eq!(set.command, ["cat", "/workspace/message"]);
    let names: Vec<&str> = set.containers.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["main", "a", "b"]);
    let b = &set.containers[2];
    assert_eq!(b.image.as_deref(), Some("busybox"));
    assert_eq!(
        (b.after.as_slice(), b.workdir.as_deref()),
        (&["a".to_owned()][..], Some("/tmp"))
    );
    assert!(
        set.containers
            .iter()
            .all(|container| container.attempts == 3)
    );
    // The task asks for what all containers request together, as the pod does.
    assert_eq!(set.resources.cpus, Some(1.5));
    assert_eq!(set.resources.memory_bytes, Some(2 << 30));
    assert!(
        set.outputs.contains(&"/workspace/".to_owned()),
        "{:?}",
        set.outputs
    );
    assert_eq!(ran.task("after").command, ["echo", "a\nb"]);
}

#[test]
fn selects_set_failure() {
    let setup = Setup {
        containers: vec![("set", vec![("b", 2)])],
        ..Setup::default()
    };
    let ran = run_with(&workflow(CONTAINER_SET), setup);
    assert!(
        ran.error().contains("b: Error (exit code 2)"),
        "{}",
        ran.error()
    );
    assert_eq!(ran.names(), ["set"]);
}

#[test]
fn permits_artifact_updates() {
    let text = r#"apiVersion: argoproj.io/v1alpha1
kind: Workflow
metadata: {generateName: test-}
spec:
  entrypoint: main
  volumeClaimTemplates:
  - metadata: {name: work}
    spec: {accessModes: [ReadWriteOnce], resources: {requests: {storage: 1Mi}}}
  templates:
  - name: main
    steps:
    - - {name: write, template: write}
    - - {name: append, template: append}
    - - {name: show, template: show, arguments: {artifacts: [{name: data, from: "{{steps.append.outputs.artifacts.data}}"}]}}
  - name: write
    container: {image: alpine, command: [sh, -c, "echo one > /work/data.txt"], volumeMounts: [{name: work, mountPath: /work}]}
  - name: append
    container: {image: alpine, command: [sh, -c, "echo two >> /work/data.txt"], volumeMounts: [{name: work, mountPath: /work}]}
    outputs: {artifacts: [{name: data, path: /work/data.txt}]}
  - name: show
    inputs: {artifacts: [{name: data, path: /tmp/data.txt}]}
    container: {image: alpine, command: [cat, /tmp/data.txt]}
"#;
    let ran = run(text);
    ran.succeeded();
    let writable = |name: &str, path: &str| {
        let task = ran.task(name);
        let input = task.inputs.iter().find(|input| input.path == path);
        input.map(|input| input.writable)
    };
    let copy = Some(workflow_graph::Writable::Copy);
    assert_eq!(writable("[1].append", "/work/data.txt"), copy);
    assert_eq!(writable("[2].show", "/tmp/data.txt"), copy);
}

#[test]
fn warns_kubernetes_fields() {
    let ran = run(&workflow(
        "  - name: main\n    nodeSelector: {disk: ssd}\n    container: {image: alpine, securityContext: {runAsUser: 1000, privileged: true}}\n",
    ));
    ran.succeeded();
    let warning =
        "nodeSelector is left out: it only tells Kubernetes how to run pods (decision 18)";
    assert!(
        ran.warnings.iter().any(|known| known == warning),
        "{:?}",
        ran.warnings
    );
    let warning = "securityContext.privileged is left out: containers run with the least \
                   permissions, only runAsUser and runAsGroup count (decision 28)";
    assert!(
        ran.warnings.iter().any(|known| known == warning),
        "{:?}",
        ran.warnings
    );
}

#[test]
fn respects_container_identity() {
    let ran = run(r#"apiVersion: argoproj.io/v1alpha1
kind: Workflow
metadata:
  generateName: test-
spec:
  entrypoint: main
  securityContext: {runAsUser: 1000, runAsGroup: 3000}
  templates:
  - name: main
    steps:
    - - {name: own, template: own}
      - {name: pod, template: pod}
  - name: own
    securityContext: {runAsUser: 2000}
    container: {image: alpine, command: ["true"], securityContext: {runAsUser: 0}}
  - name: pod
    script: {image: alpine, command: [sh, -c], source: echo hi}
"#);
    ran.succeeded();
    let run_as = |name: &str| ran.task(name).run_as;
    let given = |user, group| Some(workflow_graph::RunAs { user, group });
    assert_eq!(run_as("[0].own"), given(Some(0), Some(3000)));
    assert_eq!(run_as("[0].pod"), given(Some(1000), Some(3000)));
    // Argo writes the script with mode 0755, so that `sh -c` may run it.
    let script = ran
        .task("[0].pod")
        .inputs
        .iter()
        .find(|input| input.path == "/argo/staging/script");
    assert!(script.is_some_and(|script| script.executable));
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

/// A ConfigMap that installs an executor plugin, as `argo executor-plugin build` makes it.
fn plugin_config_map(name: &'static str, image: &str) -> &'static str {
    let text = format!(
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: {name}-executor-plugin\n  labels: {{workflows.argoproj.io/configmap-type: ExecutorPlugin}}\ndata:\n  sidecar.container: |\n    name: {name}-executor-plugin\n    image: {image}\n    command: [python, -c]\n    args: [serve]\n    ports: [{{containerPort: 4355}}]\n"
    );
    Box::leak(text.into_boxed_str())
}

#[test]
fn calls_executor_plugins() {
    // Workflow-level plugins; a simulated run does not know the reply.
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

    // Installed plugins: the one named like the template's key goes first; one that does not
    // take the template passes it on.
    let ran = run_with(
        &plugin_workflow(""),
        Setup {
            library: vec![
                plugin_config_map("hello", "hello:1"),
                plugin_config_map("another", "another:1"),
            ],
            files: vec![
                ("/argo/plugin/status", vec!["404", "200", "200"]),
                (
                    "/argo/plugin/reply.json",
                    vec![
                        "",
                        r#"{"node": {"phase": "Succeeded", "outputs": {"parameters": [{"name": "greeting", "value": "hi"}]}}}"#,
                    ],
                ),
            ],
            ..Setup::default()
        },
    );
    ran.succeeded();
    let images: Vec<Option<&str>> = ran
        .tasks()
        .iter()
        .map(|task| task.image.as_deref())
        .collect();
    assert_eq!(
        images,
        [Some("hello:1"), Some("another:1"), Some("busybox")]
    );
    assert_eq!(ran.upstream("[1].show"), ["[0].ask"]);
    assert_eq!(ran.task("[1].show").command, ["echo", "hi"]);

    // Without plugins no one runs the template, as in Argo.
    let ran = run(&plugin_workflow(""));
    assert!(
        ran.error().contains("no plugin executed the template"),
        "{}",
        ran.error()
    );
}

/// A workflow of `templates` after `documents`, like ConfigMaps, in one file.
fn with_documents(documents: &str, templates: &str) -> String {
    format!("{documents}---\n{}", workflow(templates))
}

#[test]
fn shares_volume_files() {
    let text = r#"
  - name: main
    steps:
    - - {name: write, template: write}
    - - {name: read, template: read}
    - - {name: host, template: host}
  - name: write
    volumes:
    - {name: work, persistentVolumeClaim: {claimName: shared}}
    container:
      image: alpine
      command: [sh, -c, "echo hi > /work/a.txt"]
      volumeMounts: [{name: work, mountPath: /work}]
  - name: read
    volumes:
    - {name: data, persistentVolumeClaim: {claimName: shared}}
    container:
      image: alpine
      command: [cat, /data/a.txt]
      volumeMounts: [{name: data, mountPath: /data, readOnly: true}]
  - name: host
    volumes:
    - {name: reference, hostPath: {path: /no/such/folder, type: Directory}}
    container:
      image: alpine
      command: [ls, /reference]
      volumeMounts: [{name: reference, mountPath: /reference}]
"#;
    let ran = run(&workflow(text));
    ran.succeeded();
    let write = ran.task("[0].write");
    assert!(
        write.outputs.contains(&"/work/".to_owned()),
        "{:?}",
        write.outputs
    );
    let read = ran.task("[1].read");
    assert!(
        read.inputs.iter().any(|input| input.path == "/data/a.txt"),
        "{:?}",
        read.inputs
    );
    assert!(read.outputs.is_empty(), "a read-only mount is no output");
    assert_eq!(ran.upstream("[1].read"), ["[0].write"]);
    // A host path is read on the machine that runs iunctus; a simulated run needs no such folder.
    let host = ran.task("[2].host");
    assert!(
        host.inputs
            .iter()
            .any(|input| input.path == "/reference" && input.source == "/no/such/folder"),
        "{:?}",
        host.inputs
    );
    assert!(
        ran.warnings
            .iter()
            .any(|warning| warning.contains("volume claim shared exists only in a cluster"))
    );
    // A real run needs the host path.
    let real = run_with(&workflow(text), real());
    assert!(
        real.error()
            .contains("mounts the host path /no/such/folder, which does not exist on this machine"),
        "{}",
        real.error()
    );
}

const SETTINGS: &str = "apiVersion: v1\nkind: ConfigMap\nmetadata: {name: settings}\ndata: {genome: hg38, threads: '4'}\n";

/// A real run, which gives no placeholders for missing inputs.
fn real() -> Setup {
    Setup {
        real_inputs: true,
        ..Setup::default()
    }
}

#[test]
fn mirrors_volume_removals() {
    let text = r#"apiVersion: argoproj.io/v1alpha1
kind: Workflow
metadata: {generateName: test-}
spec:
  entrypoint: main
  volumeClaimTemplates:
  - metadata: {name: work}
    spec: {accessModes: [ReadWriteOnce], resources: {requests: {storage: 1Mi}}}
  templates:
  - name: main
    steps:
    - - {name: make, template: make}
    - - {name: remove, template: remove}
    - - {name: list, template: list}
  - name: make
    container: {image: alpine, command: [sh, -c, "touch /work/a /work/b"], volumeMounts: [{name: work, mountPath: /work}]}
  - name: remove
    container: {image: alpine, command: [sh, -c, "rm /work/a"], volumeMounts: [{name: work, mountPath: /work}]}
  - name: list
    container: {image: alpine, command: [ls, /work], volumeMounts: [{name: work, mountPath: /work}]}
"#;
    let mounts = Setup {
        mounts: true,
        ..real()
    };
    let ran = run_with(text, mounts);
    ran.succeeded();
    let listed = &ran.listings["[2].list"];
    assert_eq!(listed, &["/work/b"], "Argo lists only b after rm /work/a");
}

#[test]
fn passes_repository_keys() {
    let text = r#"apiVersion: argoproj.io/v1alpha1
kind: Workflow
metadata: {generateName: key-only-}
spec:
  entrypoint: main
  artifactRepositoryRef: {key: my-key}
  templates:
  - name: main
    dag:
      tasks:
      - {name: generate, template: generate}
      - {name: consume, template: consume, depends: generate}
  - name: generate
    container: {image: alpine, command: [sh, -c, "echo hello > /mnt/file"]}
    outputs: {artifacts: [{name: file, path: /mnt/file, s3: {key: my-file}}]}
  - name: consume
    container: {image: alpine, command: [cat, /tmp/file]}
    inputs: {artifacts: [{name: file, path: /tmp/file, s3: {key: my-file}}]}
"#;
    let ran = run_with(text, real());
    ran.succeeded();
    let input = (ran.task("consume").inputs.iter())
        .find(|input| input.path == "/tmp/file")
        .expect("the input of the key")
        .clone();
    assert!(
        !input.source.contains("://"),
        "a file of the run: {input:?}"
    );
    assert!(
        ran.warnings
            .iter()
            .any(|warning| warning.starts_with("artifactRepositoryRef is left out")),
        "{:?}",
        ran.warnings
    );
    // A key that no step wrote has no value in a real run.
    let alone = text
        .replace("depends: generate", "depends: ''")
        .replace("      - {name: generate, template: generate}\n", "");
    let error = run_with(&alone, real()).error();
    assert!(
        error.contains("no step of this run wrote key my-file"),
        "{error}"
    );
}

#[test]
fn applies_pod_patches() {
    // Argo's examples pod-spec-patch-wf-tmpl.yaml: the patch of the workflow, then the
    // template's own, with the tags of the template.
    let text = r#"apiVersion: argoproj.io/v1alpha1
kind: Workflow
metadata: {generateName: pod-spec-patch-}
spec:
  entrypoint: hello-world
  arguments:
    parameters:
    - {name: cpu-limit, value: 100m}
    - {name: mem-limit, value: 100Mi}
  podSpecPatch: |
    nodeSelector: {disk: ssd}
    containers:
      - name: main
        resources:
          limits:
            memory: "{{workflow.parameters.mem-limit}}"
  templates:
  - name: hello-world
    podSpecPatch: '{"containers":[{"name":"main", "env": [{"name": "GREETING", "value": "hi"}], "resources":{"limits":{"cpu": "{{workflow.parameters.cpu-limit}}" }}}]}'
    container:
      image: busybox
      command: [echo]
      args: ["hello world"]
"#;
    let ran = run(text);
    ran.succeeded();
    let task = ran.tasks()[0];
    assert_eq!(task.resources.cpus, Some(0.1));
    assert_eq!(task.resources.memory_bytes, Some(100 << 20));
    assert_eq!(
        task.environment.get("GREETING").map(String::as_str),
        Some("hi")
    );
    assert!(
        ran.warnings
            .iter()
            .any(|warning| warning.starts_with("podSpecPatch.nodeSelector is left out")),
        "{:?}",
        ran.warnings
    );
}

#[test]
fn patches_set_main() {
    let ran = run(&workflow(
        r#"
  - name: main
    podSpecPatch: '{"containers": [{"name": "main", "env": [{"name": "GREETING", "value": "hi"}]}]}'
    containerSet:
      containers:
      - {name: first, image: alpine, command: [echo, a]}
      - {name: main, image: alpine, command: [echo, b], dependencies: [first]}
"#,
    ));
    ran.succeeded();
    let task = ran.tasks()[0];
    let main = (task.containers.iter())
        .find(|container| container.name == "main")
        .expect("the main container");
    assert_eq!(
        main.environment.get("GREETING").map(String::as_str),
        Some("hi")
    );
    assert!(
        !ran.warnings
            .iter()
            .any(|warning| warning.contains("left out")),
        "{:?}",
        ran.warnings
    );
}

#[test]
fn unresolved_when_fails() {
    // `a` is skipped, so it has no outputs; Argo cannot parse the `when` of `b` and fails it.
    let dag = r#"
  - name: main
    dag:
      tasks:
      - {name: a, template: gen, when: "false"}
      - name: b
        template: print
        depends: a
        when: "{{tasks.a.outputs.parameters.p}} == x"
        arguments: {parameters: [{name: message, value: b}]}
  - name: gen
    container: {image: busybox, command: [sh, -c, "echo x > /p"]}
    outputs: {parameters: [{name: p, valueFrom: {path: /p}}]}
"#;
    let ran = run_with(&workflow(dag), real());
    let error = ran.error();
    assert!(
        error.contains("failed to resolve {{tasks.a.outputs.parameters.p}}"),
        "{error}"
    );
}

#[test]
fn reads_parameter_maps() {
    let templates = r#"
  - name: main
    steps:
    - - name: show
        template: show
        arguments:
          parameters:
          - {name: genome, valueFrom: {configMapKeyRef: {name: settings, key: genome}}}
          - {name: fallback, valueFrom: {configMapKeyRef: {name: absent, key: x}, default: dflt}}
  - name: show
    inputs:
      parameters: [{name: genome}, {name: fallback}]
    container:
      image: alpine
      command: [echo, "{{inputs.parameters.genome}} {{inputs.parameters.fallback}}"]
"#;
    let ran = run(&with_documents(SETTINGS, templates));
    ran.succeeded();
    assert_eq!(ran.task("[0].show").command, ["echo", "hg38 dflt"]);
    // Without the ConfigMap, a simulated run uses a placeholder and a real run stops.
    let missing = templates.replace("name: settings, key: genome", "name: absent, key: genome");
    let ran = run(&with_documents(SETTINGS, &missing));
    assert_eq!(
        ran.task("[0].show").command,
        ["echo", "placeholder_config_map_absent_genome dflt"]
    );
    let ran = run_with(&with_documents(SETTINGS, &missing), real());
    assert!(
        ran.error()
            .contains("reads key genome of ConfigMap absent, which none of the loaded documents"),
        "{}",
        ran.error()
    );
}

#[test]
fn resolves_environment_secrets() {
    let templates = r#"
  - name: main
    container:
      image: alpine
      command: [env]
      env:
      - {name: THREADS, valueFrom: {configMapKeyRef: {name: settings, key: threads}}}
      - {name: TOKEN, valueFrom: {secretKeyRef: {name: db, key: password}}}
"#;
    let ran = run(&with_documents(SETTINGS, templates));
    ran.succeeded();
    let environment = &ran.tasks()[0].environment;
    assert_eq!(environment["THREADS"], "4");
    assert_eq!(environment["TOKEN"], "placeholder_secret_db_password");
    let ran = run_with(&with_documents(SETTINGS, templates), real());
    assert!(
        ran.error().contains(
            "environment variable TOKEN reads key password of Secret db; iunctus has no secret store yet"
        ),
        "{}",
        ran.error()
    );
}

#[test]
fn materializes_map_files() {
    let templates = r#"
  - name: main
    volumes:
    - {name: config, configMap: {name: settings, items: [{key: threads, path: threads.txt}]}}
    - {name: keys, secret: {secretName: db, items: [{key: password, path: password.txt}]}}
    container:
      image: alpine
      command: [cat, /config/threads.txt]
      volumeMounts: [{name: config, mountPath: /config}, {name: keys, mountPath: /keys}]
"#;
    let ran = run(&with_documents(SETTINGS, templates));
    ran.succeeded();
    let content = |path: &str| {
        ran.tasks()[0]
            .inputs
            .iter()
            .find(|input| input.path == path)
            .and_then(|input| input.content.clone())
    };
    assert_eq!(content("/config/threads.txt").as_deref(), Some("4"));
    assert_eq!(
        content("/keys/password.txt").as_deref(),
        Some("placeholder_secret_db_password")
    );
}

#[test]
fn rejects_cluster_claims() {
    let claim = r#"
  - name: main
    steps:
    - - {name: claim, template: claim}
    - - name: use
        template: use
        arguments: {parameters: [{name: claim, value: "{{steps.claim.outputs.parameters.name}}"}]}
  - name: claim
    resource:
      action: create
      manifest: |
        apiVersion: v1
        kind: PersistentVolumeClaim
        metadata: {name: pvc-{{workflow.name}}}
    outputs:
      parameters: [{name: name, valueFrom: {jsonPath: '{.metadata.name}'}}]
  - name: use
    inputs: {parameters: [{name: claim}]}
    volumes:
    - {name: work, persistentVolumeClaim: {claimName: "{{inputs.parameters.claim}}"}}
    container:
      image: alpine
      command: [touch, /work/done]
      volumeMounts: [{name: work, mountPath: /work}]
"#;
    let ran = run(&workflow(claim));
    ran.succeeded();
    // The claim runs no task, and the volume is the run's own: no warning about a cluster.
    assert_eq!(ran.names(), ["[1].use"]);
    assert!(
        !ran.warnings
            .iter()
            .any(|warning| warning.contains("exists only in a cluster")),
        "{:?}",
        ran.warnings
    );
    let deployment = claim.replace("kind: PersistentVolumeClaim", "kind: Deployment");
    let ran = run(&workflow(&deployment));
    assert!(ran.names().is_empty());
    assert!(
        matches!(&ran.execution.error, Some(error) if error.unsupported()),
        "{:?}",
        ran.execution.error
    );
    assert!(ran.error().contains("decision 18"), "{}", ran.error());
}

#[test]
fn accepts_readiness_probes() {
    let probe = "readinessProbe: {exec: {command: [sh, -c, 'exit 1']}, initialDelaySeconds: 3600}";
    for (template, tasks) in [
        (
            format!(
                "  - name: main\n    container: {{image: alpine, command: [echo, ready], {probe}}}\n    initContainers: [{{name: init, image: alpine, command: [echo, ready], {probe}}}]\n"
            ),
            2,
        ),
        (
            format!(
                "  - name: main\n    script: {{image: alpine, command: [sh], source: echo ready, {probe}}}\n"
            ),
            1,
        ),
        (
            format!(
                "  - name: main\n    containerSet: {{containers: [{{name: main, image: alpine, command: [echo, ready], {probe}}}]}}\n"
            ),
            1,
        ),
    ] {
        let ran = run(&workflow(&template));
        ran.succeeded();
        assert_eq!(ran.tasks().len(), tasks);
    }
}

#[test]
fn orders_init_containers() {
    let templates = r#"
  - name: main
    steps:
    - - {name: step, template: launch}
  - name: launch
    volumes:
    - {name: launcher, emptyDir: {}}
    initContainers:
    - name: copy
      image: alpine
      command: [sh, -c, "echo run > /launcher/launch"]
      volumeMounts: [{name: launcher, mountPath: /launcher}]
    - name: check
      image: alpine
      command: [ls, /launcher]
      mirrorVolumeMounts: true
    container:
      image: alpine
      command: [/launcher/launch]
      volumeMounts: [{name: launcher, mountPath: /launcher}]
"#;
    let ran = run(&workflow(templates));
    ran.succeeded();
    assert_eq!(
        ran.names(),
        ["[0].step (init copy)", "[0].step (init check)", "[0].step"]
    );
    assert_eq!(
        ran.upstream("[0].step (init check)"),
        ["[0].step (init copy)"]
    );
    assert_eq!(
        ran.upstream("[0].step"),
        ["[0].step (init check)", "[0].step (init copy)"]
    );
    // What the first init container wrote into the emptyDir reaches the next tasks.
    for name in ["[0].step (init check)", "[0].step"] {
        assert!(
            ran.task(name)
                .inputs
                .iter()
                .any(|input| input.path == "/launcher/launch"),
            "{name}: {:?}",
            ran.task(name).inputs
        );
    }
    assert!(
        !ran.warnings
            .iter()
            .any(|warning| warning.contains("initContainers")),
        "{:?}",
        ran.warnings
    );
    // A failed init container fails the node, and the main task does not run.
    let failed = run_with(
        &workflow(templates),
        Setup {
            exit: vec![("launch", vec![1])],
            ..Setup::default()
        },
    );
    assert_eq!(failed.names(), ["[0].step (init copy)"]);
    assert!(
        failed.error().contains("copy: Error (exit code 1)"),
        "{}",
        failed.error()
    );
}

const GREETER: &str = r#"
apiVersion: argoproj.io/v1alpha1
kind: WorkflowTemplate
metadata: {name: greeter}
spec:
  entrypoint: main
  arguments: {parameters: [{name: message}]}
  templates:
  - name: main
    steps:
    - - {name: say, template: say}
  - name: say
    container: {image: alpine, command: [echo, "{{workflow.parameters.message}}"]}
"#;

#[test]
fn starts_child_workflows() {
    let templates = r#"
  - name: main
    steps:
    - - {name: before, template: print, arguments: {parameters: [{name: message, value: start}]}}
    - - {name: start, template: start}
    - - name: after
        template: print
        arguments:
          parameters:
          - {name: message, value: "{{steps.start.outputs.parameters.name}} {{steps.start.outputs.parameters.phase}}"}
  - name: start
    resource:
      action: create
      successCondition: status.phase == Succeeded
      failureCondition: status.phase in (Failed, Error)
      manifest: |
        apiVersion: argoproj.io/v1alpha1
        kind: Workflow
        metadata: {generateName: child-}
        spec:
          arguments: {parameters: [{name: message, value: hello}]}
          workflowTemplateRef: {name: greeter}
    outputs:
      parameters:
      - {name: name, valueFrom: {jsonPath: '{.metadata.name}'}}
      - {name: phase, valueFrom: {jsonPath: '{.status.phase}'}}
"#;
    let setup = || Setup {
        library: vec![GREETER],
        ..Setup::default()
    };
    let ran = run_with(&workflow(templates), setup());
    ran.succeeded();
    assert_eq!(ran.names(), ["[0].before", "[0].say", "[2].after"]);
    assert_eq!(ran.task("[0].say").command, ["echo", "hello"]);
    assert!(ran.task("[0].say").name.starts_with("child-"));
    assert_eq!(ran.upstream("[0].say"), ["[0].before"]);
    assert_eq!(ran.upstream("[2].after"), ["[0].say"]);
    let said = &ran.task("[2].after").command;
    assert!(
        said[1].starts_with("child-") && said[1].ends_with(" Succeeded"),
        "{said:?}"
    );
    // A child that fails fails the resource node.
    let failed = run_with(
        &workflow(templates),
        Setup {
            exit: vec![("say", vec![1])],
            ..setup()
        },
    );
    assert!(
        failed
            .error()
            .contains("failure condition 'status.phase in (Error,Failed)' evaluated true"),
        "{}",
        failed.error()
    );
    // A workflow that starts itself stops at the nesting limit.
    let again = r#"
apiVersion: argoproj.io/v1alpha1
kind: WorkflowTemplate
metadata: {name: again}
spec:
  entrypoint: main
  templates:
  - name: main
    resource:
      action: create
      manifest: |
        apiVersion: argoproj.io/v1alpha1
        kind: Workflow
        metadata: {generateName: again-}
        spec: {workflowTemplateRef: {name: again}}
"#;
    let starting = "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {generateName: again-}\nspec: {workflowTemplateRef: {name: again}}\n";
    let endless = run_with(
        starting,
        Setup {
            library: vec![again],
            ..Setup::default()
        },
    );
    assert!(
        endless
            .error()
            .contains("workflows start other workflows more than 10 levels deep"),
        "{}",
        endless.error()
    );
    // A Workflow that the run did not start is left to a cluster.
    let get = templates
        .replace("action: create", "action: get")
        .replace("generateName: child-", "name: elsewhere");
    let ran = run_with(&workflow(&get), setup());
    assert!(
        matches!(&ran.execution.error, Some(error) if error.unsupported()),
        "{:?}",
        ran.execution.error
    );
}

#[test]
fn writes_host_paths() {
    let host = tempfile::tempdir().expect("a temporary directory");
    let templates = format!(
        r#"
  - name: main
    volumes:
    - {{name: out, hostPath: {{path: {}, type: Directory}}}}
    container:
      image: alpine
      command: [sh, -c, "echo hi > /out/a.txt"]
      volumeMounts: [{{name: out, mountPath: /out}}]
"#,
        host.path().display()
    );
    let written = host.path().join("a.txt");
    run(&workflow(&templates)).succeeded();
    assert!(
        !written.exists(),
        "a simulated run never writes to a host path"
    );
    run_with(&workflow(&templates), real()).succeeded();
    assert!(written.exists(), "a real run writes the file back");
    std::fs::remove_file(&written).expect("the file is removed");
    let read_only = templates.replace("mountPath: /out}", "mountPath: /out, readOnly: true}");
    run_with(&workflow(&read_only), real()).succeeded();
    assert!(!written.exists(), "a read-only mount is not written back");
}

#[test]
fn defaults_empty_outputs() {
    let templates = r#"
  - name: main
    steps:
    - - {name: decide, template: decide}
    - - name: run
        template: print
        arguments: {parameters: [{name: message, value: "{{steps.decide.outputs.parameters.cached}}"}]}
        when: "{{steps.decide.outputs.parameters.cached}} != true"
  - name: decide
    container: {image: alpine, command: [driver]}
    outputs:
      parameters:
      - {name: cached, valueFrom: {path: /tmp/cached, default: "false"}}
"#;
    let ran = run(&workflow(templates));
    ran.succeeded();
    assert_eq!(ran.task("[1].run").command, ["echo", "false"]);
    // A real run reads the empty file.
    let real = run_with(&workflow(templates), real());
    assert_eq!(real.names(), ["[0].decide"]);
}

#[test]
fn replaces_command_entrypoint() {
    let ran = run(&workflow(
        r#"
  - name: main
    steps:
    - - {name: a, template: a}
      - {name: b, template: b}
  - name: a
    container: {image: argoproj/argosay:v2, command: [/argosay], args: [echo, hi]}
  - name: b
    container: {image: argoproj/argosay:v2, args: [echo, hi]}
"#,
    ));
    ran.succeeded();
    // As in Kubernetes, which runs `/argosay echo hi` for both.
    assert!(ran.task("[0].a").replaces_entrypoint);
    assert!(!ran.task("[0].b").replaces_entrypoint);
}
