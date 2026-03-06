//! Checks the task-model request of a Nextflow task and the capture of `env` and `eval` outputs.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::process::Command;
use std::rc::Rc;

use nextflow_evaluator::interpreter::TaskDirective;
use nextflow_evaluator::value::{PathValue, Value};
use nextflow_runtime::task::{command_script, container, read_environment, task_request};
use nextflow_runtime::{TaskId, TaskRequest};
use task_model::{Digest, ReuseScope};

fn request(eval: &[&str], env: &[&str], container: Option<&str>) -> TaskRequest {
    let mut directives = vec![TaskDirective {
        name: "cpus".into(),
        arguments: vec![Value::integer(2)],
    }];
    if let Some(image) = container {
        directives.push(TaskDirective {
            name: "container".into(),
            arguments: vec![Value::string(image)],
        });
    }
    TaskRequest {
        id: TaskId(1),
        process: "P".into(),
        name: "P (1)".into(),
        script: "#!/bin/bash -ue\nFOO='a b'\necho hi\n".into(),
        directives,
        staged: vec![(PathValue::parse("/data/x.gz", "/"), "x.gz".into())],
        output_patterns: vec!["out.txt".into()],
        env: env.iter().map(|name| Rc::from(*name)).collect(),
        eval: eval.iter().map(|command| (*command).to_owned()).collect(),
    }
}

#[test]
fn captures_env_and_eval_outputs() {
    let request = request(&["echo \"v'1\"", "echo bad >&2; exit 2"], &["FOO"], None);
    let script = command_script(&request);
    assert!(script.starts_with(&request.script));
    let root = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(root.path().join(".command.sh"), &script).expect("writes the script");
    let status = Command::new("bash")
        .args(["-ue", ".command.sh"])
        .current_dir(root.path())
        .output()
        .expect("runs bash");
    assert!(status.status.success());
    let text =
        std::fs::read_to_string(root.path().join(".command.env")).expect("reads .command.env");
    let variables = read_environment(&text);
    assert_eq!(variables["FOO"], ("a b".to_owned(), 0));
    assert_eq!(variables["nxf_out_eval_1"], ("v'1".to_owned(), 0));
    assert_eq!(variables["nxf_out_eval_2"], ("bad".to_owned(), 2));
    assert!(!root.path().join(".command.eval").exists());
}

#[test]
fn scripts_without_captures_stay_as_they_are() {
    let request = request(&[], &[], None);
    assert_eq!(command_script(&request), request.script);
}

#[test]
fn requests() {
    let plain = request(&[], &[], None);
    let digest = |text: &str| Digest::sha256(text.as_bytes());
    let model = task_request(&plain, container(&plain), digest("script"), &[digest("x")]);
    assert_eq!(model.spec.image, None);
    assert_eq!(model.spec.arguments, ["/bin/bash", "-ue", ".command.sh"]);
    let paths: Vec<&str> = model
        .spec
        .inputs
        .iter()
        .map(|input| input.path.as_str())
        .collect();
    assert_eq!(paths, [".command.sh", "x.gz"]);
    assert_eq!(model.placement.cpus, 2);
    let tagged = request(&["true"], &[], Some("debian:stable-slim"));
    let model = task_request(
        &tagged,
        container(&tagged),
        digest("script"),
        &[digest("x")],
    );
    assert_eq!(model.scope, ReuseScope::Lineage);
    let patterns: Vec<&str> = model
        .spec
        .outputs
        .iter()
        .map(|rule| rule.pattern.as_str())
        .collect();
    assert_eq!(patterns, ["out.txt", ".command.env"]);
    let pinned = request(&[], &[], Some("debian@sha256:ab"));
    let model = task_request(
        &pinned,
        container(&pinned),
        digest("script"),
        &[digest("x")],
    );
    assert_eq!(model.scope, ReuseScope::Shared);
}

#[test]
fn environment_files() {
    let text = "\nA=one\n/A/\nB=line 1\nline 2\n/B/=exit:0\n";
    let variables = read_environment(text);
    assert_eq!(variables["A"], ("one".to_owned(), 0));
    assert_eq!(variables["B"], ("line 1\nline 2".to_owned(), 0));
}
