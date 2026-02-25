//! Tests loading: run links, packed documents, shared processes, cycles, step checks and errors.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::Arc;

use cwl_parser::model::{Class, Process, Step, Version};
use cwl_parser::source::Memory;
use cwl_parser::{Error, Loader};

const TOOL: &str = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: cat\n\
                    inputs:\n  file: File\noutputs:\n  out: stdout\n";

const WORKFLOW: &str = "cwlVersion: v1.2\nclass: Workflow\ninputs: {file: File}\n\
                        outputs:\n  out: {type: File, outputSource: second/out}\nsteps:\n  \
                        first: {run: tool.cwl, in: {file: file}, out: [out]}\n  \
                        second: {run: tool.cwl, in: {file: first/out}, out: [out]}\n";

fn loader(files: &[(&str, &str)]) -> Loader {
    let mut memory = Memory::default();
    for (url, text) in files {
        memory.insert(url, text);
    }
    Loader::new(memory)
}

fn load(files: &[(&str, &str)], url: &str) -> Arc<Process> {
    match loader(files).load(url) {
        Ok(document) => document.process,
        Err(error) => panic!("{error}"),
    }
}

fn failure(files: &[(&str, &str)], url: &str) -> Error {
    match loader(files).load(url) {
        Ok(document) => panic!("loaded {}", document.process.id),
        Err(error) => error,
    }
}

fn steps(process: &Process) -> &[Step] {
    match &process.class {
        Class::Workflow(workflow) => &workflow.steps,
        _ => panic!("{} is not a workflow", process.id),
    }
}

#[test]
fn links_run_targets() {
    let files = [
        ("file:///work/wf.cwl", WORKFLOW),
        ("file:///work/tool.cwl", TOOL),
    ];
    let workflow = load(&files, "file:///work/wf.cwl");
    let [first, second] = steps(&workflow) else {
        panic!("two steps");
    };
    assert_eq!(first.run.reference, "file:///work/tool.cwl");
    assert_eq!(first.run.process.id, "file:///work/tool.cwl");
    assert!(
        Arc::ptr_eq(&first.run.process, &second.run.process),
        "the tool is shared"
    );
    assert_eq!(second.inputs[0].source, ["file:///work/wf.cwl#first/out"]);
    assert_eq!(
        workflow.outputs[0].output_source,
        ["file:///work/wf.cwl#second/out"]
    );
    assert_eq!(workflow.version, Version::V1_2);
}

#[test]
fn selects_packed_processes() {
    let packed = "cwlVersion: v1.2\n$graph:\n- class: CommandLineTool\n  id: tool\n  \
                  inputs: {file: File}\n  outputs: {out: stdout}\n- class: Workflow\n  id: main\n  \
                  inputs: {file: File}\n  outputs: []\n  steps:\n    only: \
                  {run: '#tool', in: {file: file}, out: [out]}\n";
    let files = [("file:///work/packed.cwl", packed)];
    let main = load(&files, "file:///work/packed.cwl");
    assert_eq!(main.id, "file:///work/packed.cwl#main");
    let [only] = steps(&main) else {
        panic!("one step");
    };
    assert_eq!(only.run.reference, "file:///work/packed.cwl#tool");
    assert_eq!(only.id, "file:///work/packed.cwl#main/only");
    let tool = load(&files, "file:///work/packed.cwl#tool");
    assert!(matches!(tool.class, Class::CommandLineTool(_)));
}

#[test]
fn validates_graph_entries() {
    let packed = "cwlVersion: v1.2\n$graph:\n- {class: CommandLineTool, id: a, inputs: [], outputs: []}\n\
                  - {class: ExpressionTool, id: b, inputs: [], outputs: [], expression: '${return {};}'}\n";
    let files = [("file:///work/packed.cwl", packed)];
    let error = failure(&files, "file:///work/packed.cwl");
    assert_eq!(
        error.message,
        "`file:///work/packed.cwl` holds several processes but none named `#main`"
    );
    let entries = loader(&files).load_entries("file:///work/packed.cwl");
    let ids: Vec<String> = entries
        .unwrap_or_else(|error| panic!("{error}"))
        .iter()
        .map(|process| process.id.clone())
        .collect();
    assert_eq!(
        ids,
        ["file:///work/packed.cwl#a", "file:///work/packed.cwl#b"]
    );
}

#[test]
fn reports_error_locations() {
    let error = failure(&[("file:///work/wf.cwl", WORKFLOW)], "file:///work/wf.cwl");
    assert_eq!(
        error.to_string(),
        "/work/wf.cwl:7:16: `run` refers to `file:///work/tool.cwl`, which does not exist"
    );
    let broken = TOOL.replace("baseCommand: cat", "baseCommand: cat\ncolour: red");
    let files = [
        ("file:///work/wf.cwl", WORKFLOW),
        ("file:///work/tool.cwl", broken.as_str()),
    ];
    let error = failure(&files, "file:///work/wf.cwl");
    assert_eq!(
        error.to_string(),
        "/work/tool.cwl:4:1: the process has no field `colour`"
    );
}

#[test]
fn rejects_run_cycles() {
    let nested = "cwlVersion: v1.2\nclass: Workflow\n\
                  requirements: [{class: SubworkflowFeatureRequirement}]\ninputs: []\noutputs: []\n\
                  steps:\n  again: {run: OTHER, in: [], out: []}\n";
    let first = nested.replace("OTHER", "b.cwl");
    let second = nested.replace("OTHER", "a.cwl");
    let files = [
        ("file:///work/a.cwl", first.as_str()),
        ("file:///work/b.cwl", second.as_str()),
    ];
    let error = failure(&files, "file:///work/a.cwl");
    assert_eq!(error.message, "`file:///work/a.cwl` runs itself");
}

#[test]
fn checks_step_links() {
    let unknown = WORKFLOW.replace(
        "first/out}, out: [out]}",
        "first/out}, out: [out, missing]}",
    );
    let files = [
        ("file:///work/wf.cwl", unknown.as_str()),
        ("file:///work/tool.cwl", TOOL),
    ];
    let error = failure(&files, "file:///work/wf.cwl");
    assert_eq!(
        error.message,
        "the step output `missing` is not an output of the process it runs"
    );
    let unbound = WORKFLOW.replace("in: {file: file}", "in: {}");
    let files = [
        ("file:///work/wf.cwl", unbound.as_str()),
        ("file:///work/tool.cwl", TOOL),
    ];
    let error = failure(&files, "file:///work/wf.cwl");
    assert_eq!(
        error.message,
        "the step does not set the required inputs `file`"
    );
    let scattered = WORKFLOW.replace(
        "out: [out]}\n  second",
        "out: [out], scatter: file}\n  second",
    );
    let files = [
        ("file:///work/wf.cwl", scattered.as_str()),
        ("file:///work/tool.cwl", TOOL),
    ];
    let error = failure(&files, "file:///work/wf.cwl");
    assert_eq!(
        error.message,
        "the step scatters, but ScatterFeatureRequirement is missing"
    );
}

#[test]
fn reads_version_forms() {
    let prefixed = TOOL.replace("v1.2", "cwl:v1.2");
    let tool = load(
        &[("file:///work/tool.cwl", prefixed.as_str())],
        "file:///work/tool.cwl",
    );
    assert_eq!(tool.version, Version::V1_2);
    let future = TOOL.replace("v1.2", "v1.5");
    let error = failure(
        &[("file:///work/tool.cwl", future.as_str())],
        "file:///work/tool.cwl",
    );
    assert_eq!(
        error.message,
        "unsupported cwlVersion `v1.5`; supported are v1.0, v1.1 and v1.2"
    );
    let missing = TOOL.replace("cwlVersion: v1.2\n", "");
    let error = failure(
        &[("file:///work/tool.cwl", missing.as_str())],
        "file:///work/tool.cwl",
    );
    assert_eq!(error.message, "the document has no `cwlVersion`");
}
