//! Runs Galaxy tools and a Galaxy workflow with the simulator and checks their tasks, inputs
//! and outputs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use galaxy_runtime::{GalaxyDriver, Options};
use serde_json::{Value as Json, json};
use workflow_graph::{Driver, Launch, Simulator, execute};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// The folder of the tools-iuc fixtures of galaxy-parser, like `get_hrun.xml`.
fn iuc() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../parser/tests/fixtures")
}

/// The TES documents of a simulated run, in the order of the task ids.
fn tes(folder: &Path) -> Vec<Json> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(folder.join("tes"))
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    entries.sort_by_key(|path| {
        path.file_stem()
            .and_then(|stem| stem.to_str()?.parse::<u64>().ok())
    });
    (entries.iter())
        .map(|path| serde_json::from_str(&std::fs::read_to_string(path).expect("a TES file")))
        .collect::<Result<_, _>>()
        .expect("TES JSON")
}

/// The script of a task, from its TES inputs.
fn script(task: &Json) -> String {
    let inputs = task["inputs"].as_array().cloned().unwrap_or_default();
    let found = inputs.iter().find(|input| {
        input["path"]
            .as_str()
            .is_some_and(|path| path.ends_with("command.sh"))
    });
    found
        .and_then(|input| input["content"].as_str())
        .unwrap_or_default()
        .to_owned()
}

struct Simulated {
    tasks: Vec<Json>,
    results: Option<Json>,
    exported: Option<Json>,
}

fn simulate(target: PathBuf, job: Json) -> Result<Simulated, String> {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let inputs = folder.path().join("job.json");
    std::fs::write(&inputs, job.to_string()).expect("writes the job");
    let launch = Launch {
        target,
        inputs: Some(inputs),
        simulated: true,
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let options = Options {
        tools: vec![iuc()],
        ..Options::default()
    };
    let mut driver = GalaxyDriver::new(&launch, options).map_err(|error| error.message)?;
    let mut simulator = Simulator::new(folder.path());
    let execution = execute("galaxy", &mut driver, &mut simulator);
    if let Some(error) = execution.error {
        return Err(error.to_string());
    }
    Ok(Simulated {
        tasks: tes(folder.path()),
        results: driver.results(),
        exported: driver.exported(),
    })
}

#[test]
fn renders_tool_command() {
    let job = json!({"in_vcf": "in.vcf", "reference": "reference.fasta"});
    let ran = simulate(iuc().join("get_hrun.xml"), job).expect("a simulated run");
    assert_eq!(ran.tasks.len(), 1);
    let task = &ran.tasks[0];
    let script = script(task);
    assert!(
        script.contains("samtools faidx ref.fa && python '/galaxy/tool/get_hrun.py'"),
        "{script}"
    );
    assert!(
        script.contains("--out-vcf '/galaxy/job/outputs/out_vcf.vcf'"),
        "{script}"
    );
    let staged: Vec<&str> = (task["inputs"].as_array().into_iter().flatten())
        .filter_map(|input| input["path"].as_str())
        .collect();
    assert!(
        staged.contains(&"/galaxy/inputs/dataset_0.vcf"),
        "{staged:?}"
    );
    assert!(
        staged.contains(&"/galaxy/tool"),
        "the tool folder: {staged:?}"
    );
    assert_eq!(task["executors"][0]["image"], "debian:stable-slim");
    let exported = ran.exported.expect("exported outputs");
    assert_eq!(exported["out_vcf"]["class"], "File");
    assert_eq!(exported["out_vcf"]["basename"], "out_vcf.vcf");
    assert!(ran.results.is_some());
}

#[test]
fn filters_and_containers() {
    let job = json!({"input": "lines.txt", "header": "true"});
    let ran = simulate(fixture("count_lines.xml"), job).expect("a simulated run");
    let task = &ran.tasks[0];
    assert_eq!(
        task["executors"][0]["image"],
        "quay.io/biocontainers/coreutils:8.31--h14c3975_0"
    );
    let script = script(task);
    assert!(
        script
            .contains("wc -l < '/galaxy/inputs/dataset_0.txt' > '/galaxy/job/outputs/output.txt'"),
        "{script}"
    );
    assert!(script.contains("sed -i '1i lines'"), "the header: {script}");
}

#[test]
fn quotes_staged_names() {
    let job = json!({"input": "it's $(lines).txt", "header": "false"});
    let ran = simulate(fixture("count_lines.xml"), job).expect("a simulated run");
    let script = script(&ran.tasks[0]);
    assert!(
        script.contains("wc -l < '/galaxy/inputs/dataset_0.txt'"),
        "{script}"
    );
    assert!(!script.contains("it's"), "{script}");
}

#[test]
fn runs_workflow_steps() {
    let job = json!({"variants": "calls.vcf", "reference": "ref.fasta", "with_header": true});
    let ran = simulate(fixture("annotate.ga"), job).expect("a simulated run");
    assert_eq!(ran.tasks.len(), 2, "one task per tool step");
    assert_eq!(ran.tasks[0]["name"], "annotate");
    assert_eq!(ran.tasks[1]["name"], "count");
    let second = &ran.tasks[1];
    let sources: Vec<&str> = (second["inputs"].as_array().into_iter().flatten())
        .filter_map(|input| input["url"].as_str())
        .collect();
    assert!(
        sources
            .iter()
            .any(|url| url.ends_with("outputs/out_vcf.vcf")),
        "the count reads the annotated file: {sources:?}"
    );
    assert!(
        script(second).contains("sed -i"),
        "the parameter input sets header"
    );
    let exported = ran.exported.expect("exported outputs");
    assert_eq!(exported["lines"]["basename"], "output.txt");
    assert_eq!(exported["annotated"]["basename"], "out_vcf.vcf");
}

#[test]
fn describes_inputs() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let launch = |target: PathBuf| Launch {
        target,
        simulated: true,
        scratch: folder.path().to_owned(),
        ..Launch::default()
    };
    let options = Options {
        tools: vec![iuc()],
        ..Options::default()
    };
    let mut tool =
        GalaxyDriver::new(&launch(iuc().join("get_hrun.xml")), options.clone()).expect("a tool");
    let inputs = tool.inputs().expect("inputs").expect("a schema");
    assert_eq!(inputs.schema["properties"]["in_vcf"]["format"], "file-path");
    assert_eq!(inputs.schema["required"], json!(["in_vcf", "reference"]));
    let outputs = tool.outputs().expect("outputs").expect("a schema");
    assert_eq!(outputs["required"], json!(["out_vcf"]));
    let mut workflow =
        GalaxyDriver::new(&launch(fixture("annotate.ga")), options).expect("a workflow");
    let inputs = workflow.inputs().expect("inputs").expect("a schema");
    assert_eq!(
        inputs.schema["properties"]["with_header"]["type"],
        "boolean"
    );
    assert_eq!(inputs.schema["required"], json!(["variants", "reference"]));
    let outputs = workflow.outputs().expect("outputs").expect("a schema");
    assert_eq!(outputs["required"], json!(["annotated", "lines"]));
}

/// A workflow of `steps` in `folder`, beside a copy of the count_lines tool.
fn workflow(folder: &Path, steps: Json) -> PathBuf {
    let tool = folder.join("count_lines.xml");
    std::fs::copy(fixture("count_lines.xml"), tool).expect("copies the tool");
    let path = folder.join("workflow.ga");
    let text = json!({"a_galaxy_workflow": "true", "name": "w", "steps": steps});
    std::fs::write(&path, text.to_string()).expect("writes the workflow");
    path
}

#[test]
fn refuses_broken_workflows() {
    let input = json!({"id": 0, "type": "data_input", "label": "lines", "tool_state": "{}"});
    let step = |id: u64, from: u64, output: &str, label: &str| {
        json!({"id": id, "type": "tool", "tool_id": "count_lines", "tool_version": "1.0",
            "tool_state": "{}", "input_connections": {"input": {"id": from, "output_name": output}},
            "workflow_outputs": [{"output_name": "output", "label": label}]})
    };
    let cases = [
        (
            json!({"0": input, "1": step(1, 0, "output", "a"), "2": step(2, 1, "missing", "b")}),
            "step 1 has no output missing",
        ),
        (
            json!({"0": input, "1": step(1, 2, "output", "a"), "2": step(2, 1, "output", "b")}),
            "read from each other in a cycle",
        ),
        (
            json!({"0": input, "1": step(1, 0, "output", "../outside")}),
            "is not a plain file name",
        ),
    ];
    for (steps, expected) in cases {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let target = workflow(folder.path(), steps);
        let Err(error) = simulate(target, json!({"lines": "lines.txt"})) else {
            panic!("the run should fail: {expected}");
        };
        assert!(error.contains(expected), "{error}");
    }
}
