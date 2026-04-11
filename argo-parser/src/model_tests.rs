//! Tests for the typed model and the v3 to v4 normalization.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::{AnyString, Document, Template, WorkflowSpec, json_float, normalize};
use crate::decode::decode;
use crate::node::Node;
use crate::tree::parse_yaml;

fn node(text: &str) -> Node {
    parse_yaml(text).expect("valid YAML").expect("one document")
}

#[test]
fn any_strings_take_numbers_and_booleans() {
    let source = "name: t\ninputs:\n  parameters:\n  - {name: a, value: 1}\n  - {name: b, default: false}\n  - {name: c, value: 1.0, enum: [0.5, x, 2]}\n";
    let (template, unknown) = decode::<Template>(&node(source)).expect("decodes");
    assert!(unknown.is_empty());
    let parameters = &template.inputs.parameters;
    assert_eq!(parameters[0].value, Some(AnyString("1".into())));
    assert_eq!(parameters[1].default, Some(AnyString("false".into())));
    assert_eq!(parameters[2].value, Some(AnyString("1".into())));
    let choices: Vec<&str> = parameters[2]
        .enumeration
        .iter()
        .map(AnyString::as_str)
        .collect();
    assert_eq!(choices, ["0.5", "x", "2"]);
}

#[test]
fn floats_print_like_go_json() {
    assert_eq!(json_float(1.0), "1");
    assert_eq!(json_float(0.25), "0.25");
    assert_eq!(json_float(1e21), "1e+21");
    assert_eq!(json_float(1.5e-7), "1.5e-7");
    assert_eq!(json_float(-3.5), "-3.5");
}

#[test]
fn decodes_steps_and_dags() {
    let source = "entrypoint: main\ntemplates:\n- name: main\n  steps:\n  - - name: a\n      template: t\n      withItems: [1, {k: v}]\n    - name: b\n      templateRef: {name: lib, template: x, clusterScope: true}\n- name: d\n  dag:\n    failFast: false\n    tasks:\n    - {name: c, template: t, depends: a.Succeeded, continueOn: {failed: true}}\n";
    let (spec, unknown) = decode::<WorkflowSpec>(&node(source)).expect("decodes");
    assert!(unknown.is_empty(), "{unknown:?}");
    let steps = &spec.templates[0].steps;
    assert_eq!(steps[0].len(), 2);
    assert_eq!(steps[0][0].with_items.len(), 2);
    let reference = steps[0][1].template_ref.as_ref().expect("a reference");
    assert!(reference.cluster_scope);
    let dag = spec.templates[1].dag.as_ref().expect("a DAG");
    assert_eq!(dag.fail_fast, Some(false));
    assert_eq!(dag.tasks[0].depends, "a.Succeeded");
    assert!(dag.tasks[0].continue_on.expect("continueOn").failed);
}

#[test]
fn normalizes_removed_fields() {
    let mut document = node(
        "kind: CronWorkflow\nspec:\n  schedule: '* * * * *'\n  workflowSpec:\n    synchronization:\n      mutex: {name: m}\n",
    );
    normalize(&mut document);
    let spec = document.get("spec").expect("spec");
    assert!(spec.get("schedule").is_none());
    assert!(spec.get("schedules").is_some());
    let synchronization = spec
        .get("workflowSpec")
        .and_then(|workflow| workflow.get("synchronization"))
        .expect("synchronization");
    assert!(synchronization.get("mutexes").is_some());
    let (document, _) = decode::<Document>(&document).expect("decodes");
    assert_eq!(document.kind, "CronWorkflow");
}
