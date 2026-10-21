//! Rules of `template_engine:`, which iunctus renders natively for yte and in the Python
//! process for jinja2, and rules of `cwl:`, which run their tool with the CWL runtime.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::Launch;

const TEMPLATE: &str = "name: ?wildcards.sample\nthreshold: ?params.foo\nvalues:\n  ?for i in range(2):\n    - ?i * 2\n?if config[\"flag\"]:\n  flagged: true\n?else:\n  flagged: false\nlabel: plain text\n";

/// A simulated launch of a rule `render` with the action `action`.
fn launch(folder: &std::path::Path, action: &str) -> Launch {
    std::fs::write(folder.join("template.yaml"), TEMPLATE).expect("a template");
    std::fs::write(folder.join("config.yaml"), "flag: true\n").expect("a config");
    let snakefile = format!(
        "configfile: 'config.yaml'\nrule all:\n    input: 'results/a.yaml'\nrule render:\n    input: 'template.yaml'\n    output: 'results/{{sample}}.yaml'\n    params: foo=0.1\n    {action}\n"
    );
    std::fs::write(folder.join("Snakefile"), snakefile).expect("a Snakefile");
    Launch {
        target: folder.to_path_buf(),
        scratch: folder.join("scratch"),
        simulated: true,
        ..Launch::default()
    }
}

#[test]
fn renders_yte_templates() {
    let folder = tempfile::tempdir().expect("a folder");
    let launch = launch(folder.path(), "template_engine: 'yte'");
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let task = driver.start().expect("starts").remove(0);
    let [_, staged, output] = task.command.as_slice() else {
        panic!("a copy command: {:?}", task.command);
    };
    assert_eq!(output, "results/a.yaml");
    let rendered = (task.inputs.iter())
        .find(|input| &input.path == staged)
        .and_then(|input| input.content.clone())
        .expect("the rendered template");
    // Snakemake 9.27 writes this for the same template.
    let expected = "name: a\nthreshold: 0.1\nvalues:\n- 0\n- 2\nflagged: true\nlabel: plain text\n";
    assert_eq!(rendered, expected);
}

/// A simulated run renders no jinja2 template, which needs the Python process of a real one.
#[test]
fn accepts_jinja_templates() {
    let folder = tempfile::tempdir().expect("a folder");
    let launch = launch(folder.path(), "template_engine: 'jinja2'");
    let task = snakemake_runtime::driver(&launch)
        .expect("a driver")
        .start()
        .expect("starts");
    assert_eq!(task[0].command[0], "cp");
}

#[test]
fn refuses_unknown_engines() {
    let folder = tempfile::tempdir().expect("a folder");
    let launch = launch(folder.path(), "template_engine: 'mako'");
    let error = snakemake_runtime::driver(&launch)
        .expect("a driver")
        .start()
        .expect_err("an unknown engine");
    assert!(!error.unsupported, "{error}");
    assert!(
        error.message.contains("Unsupported template engine mako"),
        "{error}"
    );
}

const TOOL: &str = "cwlVersion: v1.0\nclass: CommandLineTool\nbaseCommand: sort\ninputs:\n  input:\n    type: File[]\n    inputBinding: {position: 2}\n  key:\n    type: {type: array, items: string, inputBinding: {prefix: '-k'}}\n    inputBinding: {position: 1}\n  output: string\noutputs:\n  sorted:\n    type: File\n    outputBinding: {glob: $(inputs.output)}\nstdout: $(inputs.output)\n";

/// A `cwl:` rule runs its tool with the input object of Snakemake's `cwl()`: named inputs
/// as files with absolute paths, then named params and outputs (test_cwl).
#[test]
fn runs_cwl_tools() {
    let folder = tempfile::tempdir().expect("a folder");
    std::fs::write(folder.path().join("sort.cwl"), TOOL).expect("a tool");
    std::fs::write(folder.path().join("test.txt"), "b 2\na 1\n").expect("an input");
    std::fs::write(
        folder.path().join("Snakefile"),
        "rule all:\n    input: 'sorted.txt'\nrule a:\n    input: input=['test.txt']\n    output: output='sorted.txt'\n    params: key=['2']\n    cwl: 'sort.cwl'\n",
    )
    .expect("a Snakefile");
    let launch = Launch {
        target: folder.path().to_path_buf(),
        scratch: folder.path().join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let mut executor = workflow_graph::Simulator::new(folder.path().join("made"));
    let run = workflow_graph::execute("cwl", driver.as_mut(), &mut executor);
    assert!(run.error.is_none(), "{:?}", run.error);
    let task = &run.graph.nodes[0].task;
    let command = task.command.join(" ");
    assert!(
        command.contains("sort") && command.contains("-k"),
        "{command}"
    );
    let job = folder.path().join("scratch/snakemake/cwl/1/job.json");
    let job: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(job).unwrap()).unwrap();
    let input = folder.path().join("test.txt");
    let expected = serde_json::json!({
        "input": [{"path": input.to_string_lossy(), "class": "File"}],
        "key": ["2"],
        "output": "sorted.txt",
    });
    assert_eq!(job, expected);
}

/// A `cwl:` rule in a group runs its tool as tasks of its own, and the other jobs of the
/// group, here `copy`, run after it as a group job, as Snakemake runs them in one.
#[test]
fn runs_grouped_cwl() {
    let folder = tempfile::tempdir().expect("a folder");
    std::fs::write(folder.path().join("sort.cwl"), TOOL).expect("a tool");
    std::fs::write(folder.path().join("test.txt"), "b 2\na 1\n").expect("an input");
    std::fs::write(
        folder.path().join("Snakefile"),
        "rule all:\n    input: 'copy.txt'\nrule a:\n    input: input=['test.txt']\n    output: output='sorted.txt'\n    params: key=['2']\n    group: 'g'\n    cwl: 'sort.cwl'\nrule copy:\n    input: 'sorted.txt'\n    output: 'copy.txt'\n    group: 'g'\n    shell: 'cp {input} {output}'\n",
    )
    .expect("a Snakefile");
    let launch = Launch {
        target: folder.path().to_path_buf(),
        scratch: folder.path().join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let mut executor = workflow_graph::Simulator::new(folder.path().join("made"));
    let run = workflow_graph::execute("cwl", driver.as_mut(), &mut executor);
    assert!(run.error.is_none(), "{:?}", run.error);
    let commands: Vec<String> = (run.graph.nodes.iter())
        .map(|node| node.task.command.join(" "))
        .filter(|command| !command.is_empty())
        .collect();
    assert_eq!(commands.len(), 2, "{commands:?}");
    assert!(commands[0].contains("sort"), "{commands:?}");
    assert!(
        commands[1].contains("cp sorted.txt copy.txt"),
        "{commands:?}"
    );
}

/// A `cwl:` job that runs again because its params changed runs its CWL tool, not the empty
/// task that stands for it.
#[test]
fn reruns_changed_cwl() {
    let folder = tempfile::tempdir().expect("a folder");
    std::fs::write(folder.path().join("sort.cwl"), TOOL).expect("a tool");
    std::fs::write(folder.path().join("test.txt"), "b 2\na 1\n").expect("an input");
    std::fs::write(
        folder.path().join("Snakefile"),
        "rule all:\n    input: 'sorted.txt'\nrule a:\n    input: input=['test.txt']\n    output: output='sorted.txt'\n    params: key=['2']\n    cwl: 'sort.cwl'\n",
    )
    .expect("a Snakefile");
    std::fs::write(folder.path().join("sorted.txt"), "old\n").expect("an old output");
    // The record of an earlier run, whose params differ.
    let records = folder.path().join(".snakemake/uwe-signatures");
    std::fs::create_dir_all(&records).unwrap();
    std::fs::write(records.join("c29ydGVkLnR4dA=="), "{}").unwrap();
    let launch = Launch {
        target: folder.path().to_path_buf(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let tasks = driver.start().expect("starts");
    let commands: Vec<String> = (tasks.iter()).map(|task| task.command.join(" ")).collect();
    assert!(
        commands.iter().any(|command| command.contains("sort")),
        "{commands:?}"
    );
}

/// Runs each task by writing its outputs into a read-only store, as a cache does.
struct Store(std::path::PathBuf);

impl workflow_graph::Executor for Store {
    fn run(&mut self, attempt: workflow_graph::Attempt) -> workflow_graph::Running {
        let task = &attempt.task;
        let mut files = Vec::new();
        for (number, output) in task.outputs.iter().enumerate() {
            let location = self.0.join(format!("{}-{number}", task.id));
            std::fs::write(&location, "a 1\nb 2\n").unwrap();
            let mut permissions = std::fs::metadata(&location).unwrap().permissions();
            permissions.set_readonly(true);
            std::fs::set_permissions(&location, permissions).unwrap();
            files.push(workflow_graph::OutputFile {
                path: output.clone(),
                location: location.to_string_lossy().into_owned(),
                ..Default::default()
            });
        }
        workflow_graph::Running::ended(Ok(workflow_graph::Outcome {
            id: task.id,
            files,
            ..Default::default()
        }))
    }
}

/// The outputs of a `cwl:` job in the shared directory may change, as cwltool leaves them.
#[test]
fn writes_cwl_outputs() {
    let folder = tempfile::tempdir().expect("a folder");
    let work = folder.path().join("work");
    std::fs::create_dir(&work).unwrap();
    std::fs::write(work.join("sort.cwl"), TOOL).expect("a tool");
    std::fs::write(work.join("test.txt"), "b 2\na 1\n").expect("an input");
    std::fs::write(
        work.join("Snakefile"),
        "rule all:\n    input: 'sorted.txt'\nrule a:\n    input: input=['test.txt']\n    output: output='sorted.txt'\n    params: key=['2']\n    cwl: 'sort.cwl'\n",
    )
    .expect("a Snakefile");
    let launch = Launch {
        target: work.clone(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let options = snakemake_runtime::RunSettings {
        shared_workdir: true,
        ..Default::default()
    };
    let mut driver = snakemake_runtime::driver_with_options(&launch, Default::default(), options)
        .expect("a driver");
    let store = folder.path().join("store");
    std::fs::create_dir(&store).unwrap();
    let mut executor = Store(store);
    let run = workflow_graph::Engine::new(&mut executor).run("cwl", driver.as_mut());
    assert!(run.error.is_none(), "{:?}", run.error);
    let sorted = std::fs::metadata(work.join("sorted.txt")).expect("the output");
    assert!(!sorted.permissions().readonly());
}
