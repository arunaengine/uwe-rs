//! Checks that every error has the same form, `iunctus: <where>: <what>; <what to do>`, and the
//! exit code of its kind: 1 for the workflow, 2 for the command line, 3 for the machine or its
//! config, 33 for a feature that iunctus does not support. No test needs a container engine or
//! the network: servers are at a local port that nothing listens on.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

mod common;

/// A process with an image, so that a real run only needs its executor.
const PIPELINE: &str = "process A {\n    container 'debian:stable-slim'\n    output:\n    \
                        path 'a.txt'\n    script:\n    \"echo a > a.txt\"\n}\n\nworkflow {\n    \
                        A()\n}\n";

/// Runs iunctus in `directory` with its own cache, and gives the exit code and standard error.
fn iunctus(directory: &Path, arguments: &[&str]) -> (i32, String) {
    let output = common::command()
        .args(arguments)
        .env("XDG_CACHE_HOME", directory.join("cache"))
        .current_dir(directory)
        .output()
        .expect("runs iunctus");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    (output.status.code().unwrap_or(-1), stderr)
}

fn project(script: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(directory.path().join("main.nf"), script).expect("writes the script");
    directory
}

/// Every line that does not continue the line before it names iunctus.
fn assert_form(stderr: &str) {
    for line in stderr.lines() {
        if line.is_empty() || line.starts_with(' ') {
            continue;
        }
        assert!(line.starts_with("iunctus: "), "{stderr}");
        assert!(!line.contains("(os error"), "{stderr}");
    }
}

#[test]
fn mistakes_of_the_command_line_say_so_in_one_line() {
    let directory = project(PIPELINE);
    let (code, stderr) = iunctus(directory.path(), &["run", "main.nf", "-bogus"]);
    assert_eq!(code, 2, "{stderr}");
    assert_eq!(
        stderr,
        "iunctus: unknown option `-bogus`; `iunctus help` lists the commands and their options\n"
    );
    let (code, stderr) = iunctus(directory.path(), &["launch", "main.nf"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.starts_with("iunctus: unknown command `launch`; "),
        "{stderr}"
    );
    let (code, stderr) = iunctus(directory.path(), &["run"]);
    assert_eq!(code, 2, "{stderr}");
    assert_form(&stderr);
    let output = common::command()
        .arg("help")
        .output()
        .expect("runs iunctus");
    assert_eq!(output.status.code(), Some(0));
    let usage = String::from_utf8_lossy(&output.stdout);
    assert!(usage.starts_with("usage: iunctus run"), "{usage}");
}

#[test]
fn errors_of_a_workflow_name_their_place() {
    let directory = project("process A {\n    script:\n    \"echo a\"\n\nworkflow {\n    A()\n}\n");
    let (code, stderr) = iunctus(directory.path(), &["run", "main.nf"]);
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.starts_with("iunctus: "), "{stderr}");
    assert!(stderr.contains("main.nf:8:1: "), "{stderr}");
    assert_form(&stderr);
    let directory = project("include { B } from './modules/b'\n\nworkflow {\n    B()\n}\n");
    let (code, stderr) = iunctus(directory.path(), &["run", "main.nf"]);
    assert_eq!(code, 1, "{stderr}");
    let missing = directory.path().join("modules/b.nf");
    assert!(
        stderr.contains(&format!(
            "main.nf:1:11: the included file {} does not exist",
            missing.display()
        )),
        "{stderr}"
    );
    let (code, stderr) = iunctus(directory.path(), &["run", "nowhere.nf"]);
    assert_eq!(code, 1, "{stderr}");
    assert_eq!(
        stderr,
        "iunctus: nowhere.nf: no such file or directory; name a workflow file or a project \
         directory\n"
    );
}

#[test]
fn unsupported_features_exit_with_33() {
    let script = "nextflow.enable.dsl=1\nprocess A {\n    output:\n    file 'x' into ch\n    \
                  script:\n    \"touch x\"\n}\n";
    let directory = project(script);
    let (code, stderr) = iunctus(directory.path(), &["run", "main.nf"]);
    assert_eq!(code, 33, "{stderr}");
    assert!(
        stderr.contains("DSL1 scripts are not supported (decision 14)"),
        "{stderr}"
    );
    assert_form(&stderr);
}

#[test]
fn a_broken_config_file_is_a_problem_of_the_setup() {
    let directory = project(PIPELINE);
    std::fs::write(
        directory.path().join("lab.toml"),
        "[stroe]\nlocation = \"x\"\n",
    )
    .expect("writes the config file");
    let arguments = ["run", "main.nf", "-iunctus-config", "lab.toml"];
    let (code, stderr) = iunctus(directory.path(), &arguments);
    assert_eq!(code, 3, "{stderr}");
    assert!(
        stderr.contains("iunctus has no section `stroe`"),
        "{stderr}"
    );
    let arguments = ["run", "main.nf", "-iunctus-config", "none.toml"];
    let (code, stderr) = iunctus(directory.path(), &arguments);
    assert_eq!(code, 3, "{stderr}");
    assert_form(&stderr);
}

#[test]
fn executors_that_cannot_run_say_what_to_install_or_set() {
    let directory = project(PIPELINE);
    let output = common::command()
        .args(["run", "main.nf", "-executor", "local"])
        .env("XDG_CACHE_HOME", directory.path().join("cache"))
        .env("IUNCTUS_ENGINE", "/nowhere/podman.sock")
        .current_dir(directory.path())
        .output()
        .expect("runs iunctus");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(3), "{stderr}");
    assert!(
        stderr.contains("no container engine answers at /nowhere/podman.sock; "),
        "{stderr}"
    );
    let output = common::command()
        .args(["run", "main.nf", "-executor", "slurm"])
        .env("XDG_CACHE_HOME", directory.path().join("cache"))
        .env("PATH", directory.path())
        .current_dir(directory.path())
        .output()
        .expect("runs iunctus");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(3), "{stderr}");
    assert!(
        stderr.contains("`sbatch`, `squeue` and `scancel` are not on the PATH"),
        "{stderr}"
    );
    let arguments = ["run", "main.nf", "-executor", "tes"];
    let (code, stderr) = iunctus(
        directory.path(),
        &[&arguments[..], &["-tes-server", "http://127.0.0.1:1"]].concat(),
    );
    assert_eq!(code, 3, "{stderr}");
    assert!(
        stderr.starts_with("iunctus: the TES server at http://127.0.0.1:1 does not answer: "),
        "{stderr}"
    );
    let (code, stderr) = iunctus(
        directory.path(),
        &["run", "main.nf", "-executor", "kubernetes"],
    );
    assert_eq!(code, 3, "{stderr}");
    assert!(
        stderr.contains("name it with -kubeconfig <file>"),
        "{stderr}"
    );
}
