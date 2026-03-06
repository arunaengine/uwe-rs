//! Runs the wrapper with the host's Bash and checks the control files and the manifest.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use executor::manifest::{Collected, parse};
use executor::wrapper::wrapper;
use task_model::{EnvironmentValue, FORMAT, OutputRule, TaskSpec};

fn spec(outputs: &[&str]) -> TaskSpec {
    let mut environment = BTreeMap::new();
    environment.insert(
        "GREETING".to_owned(),
        EnvironmentValue::Literal("it's me".to_owned()),
    );
    environment.insert("TASK_DIR".to_owned(), EnvironmentValue::WorkDirectory);
    TaskSpec {
        format: FORMAT,
        namespace: String::new(),
        image: None,
        arguments: vec!["/bin/bash".into(), "-ue".into(), "task script.sh".into()],
        environment,
        inputs: Vec::new(),
        outputs: outputs
            .iter()
            .map(|pattern| OutputRule {
                pattern: (*pattern).to_owned(),
            })
            .collect(),
        stdin: None,
        stdout: ".command.out".into(),
        stderr: ".command.err".into(),
    }
}

/// Writes the task script into the work directory and runs the wrapper; the result has the
/// wrapper's exit code.
fn run(root: &Path, spec: &TaskSpec, script: &str) -> i32 {
    let work = root.join("work");
    let control = root.join("control");
    std::fs::create_dir_all(&work).expect("creates the work directory");
    std::fs::write(work.join("task script.sh"), script).expect("writes the script");
    let text = wrapper(spec, &work.to_string_lossy(), &control.to_string_lossy());
    let status = Command::new("bash")
        .arg("-c")
        .arg(text)
        .status()
        .expect("runs bash");
    status.code().expect("an exit code")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).expect("reads the file")
}

#[test]
fn collects_outputs_after_success() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let script = "echo \"$GREETING in ${TASK_DIR##*/}\"\necho oops >&2\necho hi > out.txt\n\
                  mkdir -p dir/a && echo b > dir/a/b.txt\necho l > x.log\necho s > .hidden.log\n";
    let spec = spec(&["out.txt", "dir", "*.log", "missing.txt"]);
    assert_eq!(run(root.path(), &spec, script), 0);
    let work = root.path().join("work");
    let control = root.path().join("control");
    assert_eq!(read(&control.join("exit")), "0\n");
    assert_eq!(read(&work.join(".command.out")), "it's me in work\n");
    assert_eq!(read(&work.join(".command.err")), "oops\n");
    let files = parse(&read(&control.join("manifest"))).expect("parses");
    let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            ".command.out",
            ".command.err",
            "out.txt",
            "dir/a/b.txt",
            ".hidden.log",
            "x.log"
        ]
    );
    assert_eq!(
        files[2],
        Collected {
            path: "out.txt".into(),
            size: 3
        }
    );
}

#[test]
fn keeps_only_the_logs_of_a_failure() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let spec = spec(&["out.txt"]);
    assert_eq!(run(root.path(), &spec, "echo hi > out.txt\nexit 3\n"), 0);
    let control = root.path().join("control");
    assert_eq!(read(&control.join("exit")), "3\n");
    let files = parse(&read(&control.join("manifest"))).expect("parses");
    let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, [".command.out", ".command.err"]);
}

#[test]
fn manifest_errors() {
    assert!(parse("12 a.txt\n12 a.txt\n").is_ok_and(|files| files.len() == 1));
    let error = parse("x a.txt\n").unwrap_err();
    assert_eq!(
        error.to_string(),
        "line 1 of the result manifest is not `<size> <path>`: x a.txt"
    );
}
