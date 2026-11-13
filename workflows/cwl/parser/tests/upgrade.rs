//! Tests the upgrade of v1.0 and development versions to the v1.2 model, rule by rule.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::Arc;

use cwl_parser::Loader;
use cwl_parser::model::{
    Class, Dynamic, LoadListing, PickValue, Process, RequirementClass, Version,
};
use cwl_parser::source::Memory;

const URL: &str = "file:///work/doc.cwl";

fn load(text: &str) -> Arc<Process> {
    let mut memory = Memory::default();
    memory.insert(URL, text);
    match Loader::new(memory).load(URL) {
        Ok(document) => document.process,
        Err(error) => panic!("{error}"),
    }
}

/// The requirement classes of a process, named as in the model.
fn classes(process: &Process) -> Vec<String> {
    let name = |class: &RequirementClass| match class {
        RequirementClass::NetworkAccess { network_access } => {
            format!("NetworkAccess {network_access:?}")
        }
        RequirementClass::LoadListing { load_listing } => format!("LoadListing {load_listing:?}"),
        RequirementClass::ToolTimeLimit { timelimit } => format!("ToolTimeLimit {timelimit:?}"),
        other => format!("{other:?}")
            .chars()
            .take_while(|c| c.is_alphanumeric())
            .collect(),
    };
    process
        .requirements
        .iter()
        .map(|requirement| name(&requirement.class))
        .collect()
}

const TOOL: &str = "cwlVersion: v1.0\nclass: CommandLineTool\ninputs: []\noutputs: []\n";

#[test]
fn adds_network_access() {
    let tool = load(TOOL);
    assert_eq!(tool.version, Version::V1_0);
    assert_eq!(
        classes(&tool),
        ["NetworkAccess Value(true)", "LoadListing Some(DeepListing)"]
    );
    assert!(classes(&load(&TOOL.replace("v1.0", "v1.2"))).is_empty());
    let expression = "cwlVersion: v1.0\nclass: ExpressionTool\ninputs: []\noutputs: []\n\
                      expression: $({})\n";
    assert!(classes(&load(expression)).is_empty());
}

#[test]
fn keeps_declared_access() {
    let hinted = format!(
        "{TOOL}$namespaces: {{cwltool: 'http://commonwl.org/cwltool#'}}\n\
         hints:\n  - {{class: NetworkAccess, networkAccess: false}}\n  \
         - {{class: 'cwltool:LoadListingRequirement', loadListing: no_listing}}\n"
    );
    let tool = load(&hinted);
    assert!(classes(&tool).is_empty());
    let RequirementClass::LoadListing { load_listing } = &tool.hints[1].class else {
        panic!("the renamed hint");
    };
    assert_eq!(*load_listing, Some(LoadListing::NoListing));
}

#[test]
fn renames_extension_classes() {
    let limited = format!(
        "{TOOL}$namespaces: {{cwltool: 'http://commonwl.org/cwltool#'}}\n\
         requirements:\n  - {{class: 'cwltool:TimeLimit', timelimit: 10}}\n"
    );
    let tool = load(&limited);
    assert_eq!(classes(&tool)[0], "ToolTimeLimit Value(10)");
    let current = load(&limited.replace("v1.0", "v1.2"));
    assert!(matches!(
        current.requirements[0].class,
        RequirementClass::Other { .. }
    ));
}

#[test]
fn moves_load_contents() {
    let tool = TOOL.replace(
        "inputs: []",
        "inputs:\n  x: {type: File, inputBinding: {loadContents: true, position: 1}}",
    );
    let tool = load(&tool);
    let input = &tool.inputs[0];
    assert_eq!(input.load_contents, Some(true));
    let binding = input.binding.as_ref().expect("the binding stays on tools");
    assert_eq!(binding.load_contents, None);
    assert_eq!(binding.position, Some(Dynamic::Value(1)));
}

#[test]
fn drops_workflow_bindings() {
    let workflow = "cwlVersion: v1.0\nclass: Workflow\n\
                    inputs:\n  x: {type: File, inputBinding: {loadContents: true, position: 1}}\n\
                    outputs: []\nsteps: []\n";
    let workflow = load(workflow);
    assert_eq!(workflow.inputs[0].load_contents, Some(true));
    assert_eq!(workflow.inputs[0].binding, None);
}

#[test]
fn upgrades_inline_processes() {
    let workflow = "cwlVersion: v1.0\nclass: Workflow\ninputs: []\noutputs: []\n\
                    steps:\n  s: {run: {class: CommandLineTool, inputs: [], outputs: []}, in: [], out: []}\n";
    let workflow = load(workflow);
    let Class::Workflow(body) = &workflow.class else {
        panic!("a workflow");
    };
    assert_eq!(classes(&body.steps[0].run.process).len(), 2);
}

#[test]
fn reads_development_versions() {
    let workflow = "cwlVersion: v1.2.0-dev2\nclass: Workflow\ninputs: {a: string?}\n\
                    outputs:\n  o: {type: string, outputSource: [a], pickValue: only_non_null}\nsteps: []\n";
    let workflow = load(workflow);
    assert_eq!(workflow.version, Version::V1_2Dev2);
    assert_eq!(workflow.outputs[0].pick_value, Some(PickValue::OnlyNonNull));
}
