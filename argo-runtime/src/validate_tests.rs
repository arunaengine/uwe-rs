//! Tests for the checks of submitted documents. The expected messages are those of
//! `argo lint --offline` v4.1.4 on the same documents.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::validate;
use crate::library::{Library, load_file};

/// Validates the first document of `text`, with the documents of `others` in the library.
fn check(text: &str, others: &[&str]) -> Result<(), String> {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let library = Library::new();
    for (position, other) in others.iter().enumerate() {
        let file = directory.path().join(format!("other-{position}.yaml"));
        std::fs::write(&file, other).expect("the file is written");
        library.add_documents(1, load_file(&file).expect("the file loads"));
    }
    let file = directory.path().join("document.yaml");
    std::fs::write(&file, text).expect("the file is written");
    let documents = load_file(&file).expect("the file loads");
    library.add_documents(0, documents.iter().cloned());
    validate(&documents[0], &library).map_err(|rejection| rejection.message)
}

fn workflow(templates: &str) -> String {
    format!(
        "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {{generateName: t-}}\nspec:\n  entrypoint: main\n  templates:\n{templates}"
    )
}

fn workflow_template(name: &str, templates: &str) -> String {
    format!(
        "apiVersion: argoproj.io/v1alpha1\nkind: WorkflowTemplate\nmetadata: {{name: {name}}}\nspec:\n  templates:\n{templates}"
    )
}

const PRINT: &str = "  - name: print\n    inputs: {parameters: [{name: message}]}\n    container: {image: busybox}\n";

#[test]
fn reads_document_entrypoints() {
    let entry = |templates: &str| {
        format!(
            "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {{generateName: t-}}\nspec:\n  entrypoint: main\n  templates:\n{templates}"
        )
    };
    let undefined = entry("  - name: other\n    container: {image: busybox}\n");
    assert_eq!(
        check(&undefined, &[]),
        Err("template name 'main' undefined".into())
    );
    let unsupplied = entry(
        "  - name: main\n    inputs: {parameters: [{name: message}]}\n    container: {image: busybox}\n",
    );
    assert_eq!(
        check(&unsupplied, &[]),
        Err("templates.main inputs.parameters.message was not supplied".into())
    );
    let empty_location = entry(
        "  - name: main\n    inputs:\n      artifacts: [{name: kubectl, path: /bin/kubectl, http: {url: \"\"}}]\n    container: {image: debian}\n",
    );
    assert_eq!(
        check(&empty_location, &[]),
        Err("templates.main inputs.artifacts.kubectl was not supplied".into())
    );
    // A WorkflowTemplate checks each template on its own, which gets every input.
    assert_eq!(check(&workflow_template("lib", PRINT), &[]), Ok(()));
    let twice = workflow_template(
        "t3",
        "  - name: print\n    container: {image: busybox}\n  - name: other\n    container: {image: busybox}\n  - name: print\n    container: {image: busybox}\n",
    );
    assert_eq!(
        check(&twice, &[]),
        Err("spec.templates[2].name 'print' is not unique".into())
    );
    let typeless = workflow_template(
        "t4",
        "  - name: print\n    container: {image: busybox}\n  - name: typeless\n    inputs:\n      parameters: [{name: x}]\n",
    );
    assert_eq!(
        check(&typeless, &[]),
        Err("templates.typeless template 'typeless' type is unknown".into())
    );
    let enumeration = "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {generateName: t9-}\nspec:\n  entrypoint: main\n  arguments:\n    parameters: [{name: kind, value: x, enum: [a, b]}]\n  templates:\n  - name: main\n    container: {image: busybox}\n";
    assert_eq!(
        check(enumeration, &[]),
        Err("spec.arguments.kind.value should be present in spec.arguments.kind.enum list".into())
    );
}

#[test]
fn supplies_template_inputs() {
    let missing = workflow(&format!(
        "  - name: main\n    steps:\n    - - {{name: a, template: print}}\n{PRINT}"
    ));
    assert_eq!(
        check(&missing, &[]),
        Err(
            "templates.main.steps[0].a templates.print inputs.parameters.message was not supplied"
                .into()
        )
    );
    let given = workflow(&format!(
        "  - name: main\n    steps:\n    - - {{name: a, template: print, arguments: {{parameters: [{{name: message, value: hi}}]}}}}\n{PRINT}"
    ));
    assert_eq!(check(&given, &[]), Ok(()));
}

#[test]
fn references_resolve() {
    let undefined = workflow("  - name: main\n    steps:\n    - - {name: a, template: nope}\n");
    assert_eq!(
        check(&undefined, &[]),
        Err("templates.main.steps[0].a template name 'nope' undefined".into())
    );
    let library = workflow_template("t10", "  - name: print\n    container: {image: busybox}\n");
    let reference = workflow(
        "  - name: main\n    steps:\n    - - {name: b, templateRef: {name: t10, template: missing}}\n",
    );
    assert_eq!(
        check(&reference, &[&library]),
        Err("templates.main.steps[0].b template reference t10.missing not found".into())
    );
    let reference = reference.replace("missing", "print");
    assert_eq!(check(&reference, &[&library]), Ok(()));
    assert_eq!(
        check(&reference, &[]),
        Err("templates.main.steps[0].b couldn't find workflow template \"t10\"".into())
    );
}

#[test]
fn step_names() {
    let step_name = workflow_template(
        "t7",
        "  - name: main\n    steps:\n    - - {name: a_b, template: print}\n  - name: print\n    container: {image: busybox}\n",
    );
    assert_eq!(
        check(&step_name, &[]),
        Err("templates.main templates.main.steps[0].name 'a_b' is invalid: name must consist of alpha-numeric characters or '-', and must start with an alpha-numeric character (e.g. My-name1-2, 123-NAME)".into())
    );
}

#[test]
fn dag_tasks() {
    let dag = |tasks: &str| {
        workflow_template(
            "t5",
            &format!(
                "  - name: main\n    dag:\n      tasks:\n{tasks}  - name: print\n    container: {{image: busybox}}\n"
            ),
        )
    };
    let cycle = dag(
        "      - {name: a, template: print, depends: b}\n      - {name: b, template: print, depends: a}\n",
    );
    assert_eq!(
        check(&cycle, &[]),
        Err("templates.main templates.main sorting failed: graph with cycle".into())
    );
    let mixed = dag(
        "      - {name: a, template: print}\n      - {name: b, template: print, depends: a}\n      - {name: c, template: print, dependencies: [a]}\n",
    );
    assert_eq!(
        check(&mixed, &[]),
        Err("templates.main templates.main cannot use both 'depends' and 'dependencies' in the same DAG template".into())
    );
    let unknown = dag("      - {name: a, template: print, depends: c}\n");
    assert_eq!(
        check(&unknown, &[]),
        Err("templates.main templates.main sorting failed: invalid dependency c".into())
    );
    // An argument without a value; the defaults give the container its image.
    let text = "apiVersion: argoproj.io/v1alpha1\nkind: WorkflowTemplate\nmetadata: {name: t8}\nspec:\n  templateDefaults:\n    container: {image: busybox}\n  templates:\n  - name: main\n    dag:\n      tasks:\n      - {name: a, template: print, arguments: {parameters: [{name: message, value: hi}]}}\n      - {name: c, template: print, arguments: {parameters: [{name: message}]}}\n  - name: print\n    inputs:\n      parameters: [{name: message}]\n    container: {command: [echo]}\n";
    assert_eq!(
        check(text, &[]),
        Err("templates.main templates.main.tasks.c.arguments.message.value or templates.main.tasks.c.arguments.message.valueFrom is required".into())
    );
}

#[test]
fn leaf_templates() {
    let imageless = workflow_template("t14", "  - name: main\n    container: {command: [echo]}\n");
    assert_eq!(
        check(&imageless, &[]),
        Err("templates.main templates.main.container.image may not be empty".into())
    );
    let defaults = imageless.replace(
        "spec:\n",
        "spec:\n  templateDefaults:\n    container: {image: busybox}\n",
    );
    assert_eq!(check(&defaults, &[]), Ok(()));
}

#[test]
fn rejects_cluster_templates() {
    let resource = |kind: &str| {
        workflow(&format!(
            "  - name: main\n    steps:\n    - - {{name: make, template: make}}\n  - name: make\n    resource:\n      action: create\n      manifest: |\n        apiVersion: v1\n        kind: {kind}\n        metadata: {{name: x}}\n"
        ))
    };
    let check_document = |text: &str| {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let file = directory.path().join("document.yaml");
        std::fs::write(&file, text).expect("the file is written");
        let documents = load_file(&file).expect("the file loads");
        let library = Library::new();
        library.add_documents(0, documents.iter().cloned());
        validate(&documents[0], &library)
    };
    let rejection = check_document(&resource("Deployment")).expect_err("a Deployment");
    assert!(rejection.unsupported);
    assert_eq!(
        rejection.message,
        "templates.main.steps[0].make template make is a resource template that manages Kubernetes objects of kind Deployment; uwe runs workflows without Kubernetes and does not support this (decision 18)"
    );
    // A claim becomes a volume of the run.
    assert_eq!(check_document(&resource("PersistentVolumeClaim")), Ok(()));
    // A daemon runs on the local executor and Kubernetes.
    let daemon = workflow("  - name: main\n    daemon: true\n    container: {image: nginx}\n");
    assert_eq!(check_document(&daemon), Ok(()));
    // Argo's own checks are no question of support.
    let undefined = workflow("  - name: main\n    steps:\n    - - {name: a, template: nope}\n");
    assert!(
        !check_document(&undefined)
            .expect_err("undefined")
            .unsupported
    );
}

#[test]
fn unresolved_tags() {
    let leaf = workflow(
        "  - name: main\n    inputs: {parameters: [{name: message, value: hi}]}\n    container: {image: busybox, args: [\"echo {{inputs.parameters.mesage}}\"]}\n",
    );
    assert_eq!(
        check(&leaf, &[]),
        Err("templates.main: failed to resolve {{inputs.parameters.mesage}}".into())
    );
    let flip = "  - name: flip\n    script: {image: busybox, command: [sh], source: echo heads}\n";
    let steps = workflow(&format!(
        "  - name: main\n    steps:\n    - - {{name: flip, template: flip}}\n    - - {{name: heads, template: flip, when: \"{{{{steps.flip.outputs.reslt}}}} == heads\"}}\n{flip}"
    ));
    assert_eq!(
        check(&steps, &[]),
        Err("templates.main.steps failed to resolve {{steps.flip.outputs.reslt}}".into())
    );
    assert_eq!(check(&steps.replace("reslt", "result"), &[]), Ok(()));
    let dag = workflow(&format!(
        "  - name: main\n    dag:\n      tasks:\n      - {{name: a, template: flip}}\n      - {{name: b, template: flip, depends: a, when: \"{{{{tasks.a.status}}}} == x\"}}\n      - {{name: c, template: flip, when: \"{{{{tasks.a.status}}}} == x\"}}\n{flip}"
    ));
    assert_eq!(
        check(&dag, &[]),
        Err("templates.main.tasks.c failed to resolve {{tasks.a.status}}".into())
    );
}
