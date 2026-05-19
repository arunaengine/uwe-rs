//! Runs the wrapper with the host's Bash and checks the control files and the manifest.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use executor::manifest::{Collected, parse};
use executor::wrapper::wrapper;
use task_model::{Digest, EnvironmentValue, FORMAT, OutputRule, TaskSpec};

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
        workdir: None,
        environment,
        inputs: Vec::new(),
        outputs: outputs
            .iter()
            .map(|pattern| OutputRule {
                pattern: (*pattern).to_owned(),
            })
            .collect(),
        stdin: None,
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

fn paths(files: &[Collected]) -> Vec<&str> {
    files.iter().map(|file| file.path.as_str()).collect()
}

#[test]
fn collects_outputs_after_success() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let script = "echo \"$GREETING in ${TASK_DIR##*/}\"\necho oops >&2\necho hi > out.txt\n\
                  mkdir -p dir/a && echo b > dir/a/b.txt\necho l > x.log\necho s > .hidden.log\n";
    let spec = spec(&["out.txt", "dir/", "*.log", "missing.txt"]);
    assert_eq!(run(root.path(), &spec, script), 0);
    let control = root.path().join("control");
    assert_eq!(read(&control.join("exit")), "0\n");
    // The captured streams stay out of the work directory, where a pattern could match them.
    assert_eq!(read(&control.join("stdout")), "it's me in work\n");
    assert_eq!(read(&control.join("stderr")), "oops\n");
    let files = parse(&read(&control.join("manifest"))).expect("parses");
    assert_eq!(paths(&files), ["out.txt", "dir", ".hidden.log", "x.log"]);
    assert_eq!(
        files[..2],
        [
            Collected {
                path: "out.txt".into(),
                directory: false,
                size: 3
            },
            Collected {
                path: "dir".into(),
                directory: true,
                size: 0
            }
        ]
    );
}

#[test]
fn patterns_take_braces_globstar_and_absolute_paths() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let elsewhere = root.path().join("elsewhere");
    let script = format!(
        "mkdir -p a/b && touch a/b/c.fq top.fq r.html r.zip r.txt\n\
         mkdir -p '{0}' && echo far > '{0}/far.txt'\n",
        elsewhere.display()
    );
    let far = format!("{}/*.txt", elsewhere.display());
    let spec = spec(&["**/*.fq", "r.{html,zip}", &far]);
    assert_eq!(run(root.path(), &spec, &script), 0);
    let control = root.path().join("control");
    let files = parse(&read(&control.join("manifest"))).expect("parses");
    let far = format!("{}/far.txt", elsewhere.display());
    assert_eq!(
        paths(&files),
        ["a/b/c.fq", "top.fq", "r.html", "r.zip", far.as_str()]
    );
    // An output at an absolute path is copied below `root` in the control directory.
    let copy = control.join("root").join(far.trim_start_matches('/'));
    assert_eq!(read(&copy), "far\n");
}

#[test]
fn keeps_only_the_logs_of_a_failure() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let spec = spec(&["out.txt"]);
    assert_eq!(run(root.path(), &spec, "echo hi > out.txt\nexit 3\n"), 0);
    let control = root.path().join("control");
    assert_eq!(read(&control.join("exit")), "3\n");
    assert_eq!(read(&control.join("manifest")), "");
    assert!(control.join("stdout").is_file() && control.join("stderr").is_file());
}

#[test]
fn reads_the_standard_input() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let mut spec = spec(&["got.txt"]);
    spec.stdin = Some(Digest::sha256(b"piped\n"));
    let control = root.path().join("control");
    std::fs::create_dir_all(&control).expect("creates the control directory");
    std::fs::write(control.join("stdin"), "piped\n").expect("writes the standard input");
    assert_eq!(run(root.path(), &spec, "cat > got.txt\n"), 0);
    assert_eq!(read(&root.path().join("work/got.txt")), "piped\n");
}

#[test]
fn manifest_errors() {
    let text = "file 12 a.txt\nfile 12 a.txt\ndirectory d e\n";
    let files = parse(text).expect("parses");
    assert_eq!(paths(&files), ["a.txt", "d e"]);
    let error = parse("12 a.txt\n").unwrap_err();
    assert_eq!(
        error.to_string(),
        "line 1 of the result manifest is not `file <size> <path>` or `directory <path>`: 12 a.txt"
    );
}
