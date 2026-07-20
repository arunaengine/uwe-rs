//! Checks the container task of a Nextflow task, its outcome, and the capture of `env` and
//! `eval` outputs.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::process::Command;
use std::rc::Rc;

use nextflow_evaluator::interpreter::TaskDirective;
use nextflow_evaluator::value::{PathValue, Value};
use nextflow_runtime::driver::{task, task_outcome};
use nextflow_runtime::task::{command_script, read_environment};
use nextflow_runtime::{TaskId, TaskRequest};
use workflow_graph::{Outcome, OutputFile};

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
fn container_tasks() {
    let plain = request(&[], &[], None);
    let container = task(&plain, &BTreeMap::new());
    assert_eq!(container.image, None);
    assert_eq!(container.step, "P");
    assert_eq!(container.command, ["/bin/bash", "-ue", ".command.sh"]);
    assert_eq!(container.script.as_deref(), Some(plain.script.as_str()));
    let paths: Vec<&str> = container
        .inputs
        .iter()
        .map(|input| input.path.as_str())
        .collect();
    assert_eq!(paths, [".command.sh", "x.gz"]);
    assert_eq!(
        container.inputs[0].content.as_deref(),
        Some(plain.script.as_str())
    );
    assert_eq!(container.inputs[1].source, "/data/x.gz");
    assert_eq!(container.outputs, ["out.txt"]);
    assert_eq!(container.resources.cpus, Some(2.0));
    let tagged = request(&["true"], &[], Some("debian:stable-slim"));
    let environment = BTreeMap::from([("A".to_owned(), "1".to_owned())]);
    let container = task(&tagged, &environment);
    assert_eq!(container.image.as_deref(), Some("debian:stable-slim"));
    assert_eq!(container.outputs, ["out.txt", ".command.env"]);
    assert_eq!(container.environment, environment);
    let script = &container.inputs[0].content;
    assert!(
        script
            .as_deref()
            .is_some_and(|text| text.contains("nxf_out_eval_1"))
    );
}

#[test]
fn outcomes_carry_env_and_eval_values() {
    let request = request(&["echo v"], &["FOO"], None);
    let directory = tempfile::tempdir().expect("a temporary directory");
    let file = |name: &str, text: &str| {
        let location = directory.path().join(name);
        std::fs::write(&location, text).expect("writes a file");
        OutputFile {
            path: name.into(),
            location: location.to_string_lossy().into_owned(),
            ..OutputFile::default()
        }
    };
    let files = vec![
        file("out.txt", ""),
        file(
            ".command.env",
            "FOO=a b\n/FOO/\nnxf_out_eval_1=1.0\n/nxf_out_eval_1/=exit:0\n",
        ),
    ];
    let outcome = Outcome {
        id: workflow_graph::TaskId(1),
        files,
        stdout: "said\n".into(),
        ..Outcome::default()
    };
    let result = task_outcome(&request, outcome, None).expect("an outcome");
    let names: Vec<&str> = result.files.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["out.txt"]);
    assert_eq!(result.eval, ["1.0"]);
    assert_eq!(result.env["FOO"], "a b");
    assert_eq!(result.stdout, "said\n");
    // The simulator writes no values; they are empty.
    let outcome = Outcome {
        id: workflow_graph::TaskId(1),
        ..Outcome::default()
    };
    let result = task_outcome(&request, outcome, None).expect("an outcome");
    assert_eq!(result.eval, [""]);
    assert_eq!(result.env["FOO"], "");
    let failed = file(
        ".command.env",
        "nxf_out_eval_1=bad\n/nxf_out_eval_1/=exit:2\n",
    );
    let outcome = Outcome {
        id: workflow_graph::TaskId(1),
        files: vec![failed],
        ..Outcome::default()
    };
    let error = task_outcome(&request, outcome, None).expect_err("the eval command failed");
    assert!(error.message.contains("exit status 2: bad"), "{error}");
}

#[test]
fn environment_files() {
    let text = "\nA=one\n/A/\nB=line 1\nline 2\n/B/=exit:0\n";
    let variables = read_environment(text);
    assert_eq!(variables["A"], ("one".to_owned(), 0));
    assert_eq!(variables["B"], ("line 1\nline 2".to_owned(), 0));
}

/// A store elsewhere, whose `s3://store/` locations have their content in a local folder.
struct Elsewhere(std::path::PathBuf);

impl workflow_graph::Files for Elsewhere {
    fn local(&self, location: &str) -> std::io::Result<std::path::PathBuf> {
        let key = location
            .strip_prefix("s3://store/")
            .ok_or(std::io::ErrorKind::NotFound)?;
        Ok(self.0.join(key))
    }

    fn deliver(&self, _delivery: &workflow_graph::Delivery) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn values_come_through_the_files_of_a_store_elsewhere() {
    let request = request(&[], &["FOO"], None);
    let directory = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(directory.path().join("env"), "FOO=remote\n/FOO/\n").expect("writes");
    let outcome = Outcome {
        id: workflow_graph::TaskId(1),
        files: vec![OutputFile {
            path: ".command.env".into(),
            location: "s3://store/env".into(),
            ..OutputFile::default()
        }],
        ..Outcome::default()
    };
    let files = Elsewhere(directory.path().to_owned());
    let result = task_outcome(&request, outcome, Some(&files)).expect("an outcome");
    assert_eq!(result.env["FOO"], "remote");
}
