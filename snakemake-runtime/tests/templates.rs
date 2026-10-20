//! Rules of `template_engine:`, which iunctus renders natively for yte and in the Python
//! process for jinja2, and rules of `cwl:`, which run their tool with the CWL runtime.
// Copyright (c) 2026 The iunctus Contributors
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
