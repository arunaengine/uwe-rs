//! Runs `iunctus check` on small workflows of every language: it reports every problem that
//! would stop a real run at once, runs no task and records no run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

mod common;

/// Writes the files into a new folder.
fn folder(files: &[(&str, &str)]) -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("a temporary directory");
    for (name, text) in files {
        let path = directory.path().join(name);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("makes a folder");
        std::fs::write(path, text).expect("writes a file");
    }
    directory
}

/// Runs `iunctus check` in `directory` and gives the exit code and standard output.
fn check(directory: &Path, arguments: &[&str]) -> (i32, String) {
    let output = common::command()
        .arg("check")
        .args(arguments)
        .env("XDG_CACHE_HOME", directory.join("cache"))
        .current_dir(directory)
        .output()
        .expect("runs iunctus");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    (output.status.code().unwrap_or(-1), stdout)
}

/// A process without an image that reads a file that is not there, and one that is fine.
const PIPELINE: &str = "params.input = 'data/samples.csv'

process COPY {
    input:
    path x
    output:
    path 'y'
    script:
    \"cp $x y\"
}

process SAY {
    container 'debian:stable-slim'
    output:
    path 'z'
    script:
    \"echo z > z\"
}

workflow {
    COPY(file(params.input))
    SAY()
}
";

#[test]
fn reports_planning_problems() {
    let directory = folder(&[("main.nf", PIPELINE)]);
    let (code, stdout) = check(directory.path(), &["main.nf"]);
    assert_eq!(code, 1, "{stdout}");
    assert_eq!(
        stdout,
        "error: step COPY reads data/samples.csv, which does not exist\nerror: step COPY: 1 task \
         names no container image; iunctus runs every task in a container, so name an image in \
         the workflow or give one with `-default-image <image>`\nmain.nf (Nextflow): 2 errors \
         would stop a real run.\n"
    );
    std::fs::create_dir_all(directory.path().join("data")).expect("makes a folder");
    std::fs::write(directory.path().join("data/samples.csv"), "a\n").expect("writes");
    let arguments = ["main.nf", "-default-image", "debian:stable-slim"];
    let (code, stdout) = check(directory.path(), &arguments);
    assert_eq!(code, 0, "{stdout}");
    assert_eq!(
        stdout,
        "main.nf (Nextflow): nothing would stop a real run of 2 tasks in 2 steps.\n"
    );
    // No run was recorded, and no made-up file stayed.
    let cache = directory.path().join("cache/iunctus");
    assert!(!cache.join("runs.sqlite").exists());
    let checks = cache.join("scratch/check");
    let left = std::fs::read_dir(&checks).map_or(0, |entries| entries.count());
    assert_eq!(left, 0);
}

#[test]
fn reports_reference_problems() {
    let script = "include { B } from './modules/b'\ninclude { C } from './modules/c'\n\n\
                  workflow {\n    B()\n    C()\n}\n";
    let directory = folder(&[("main.nf", script)]);
    let (code, stdout) = check(directory.path(), &["main.nf"]);
    assert_eq!(code, 1, "{stdout}");
    assert_eq!(stdout.matches("error: ").count(), 2, "{stdout}");
    assert!(stdout.contains("modules/b.nf does not exist"), "{stdout}");
    assert!(stdout.contains("modules/c.nf does not exist"), "{stdout}");
    let dsl1 = "nextflow.enable.dsl=1\nprocess A {\n    output:\n    file 'x' into ch\n    \
                script:\n    \"touch x\"\n}\n";
    let directory = folder(&[("main.nf", dsl1)]);
    let (code, stdout) = check(directory.path(), &["main.nf"]);
    assert_eq!(code, 33, "{stdout}");
    assert!(stdout.starts_with("unsupported: "), "{stdout}");
    assert!(
        stdout.ends_with("main.nf (Nextflow): 1 unsupported feature would stop a real run.\n"),
        "{stdout}"
    );
}

#[test]
fn checks_native_languages() {
    let snakefile = "rule all:\n    input: \"out/a.txt\"\n\nrule copy:\n    input: \
                     \"data/{name}.txt\"\n    output: \"out/{name}.txt\"\n    shell: \"cp \
                     {input} {output}\"\n";
    let directory = folder(&[("Snakefile", snakefile)]);
    let (code, stdout) = check(directory.path(), &["Snakefile"]);
    assert_eq!(code, 1, "{stdout}");
    assert!(
        stdout.contains("error: step copy reads data/a.txt, which does not exist\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("error: step copy: 1 task names no container image"),
        "{stdout}"
    );
    let tool = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: cat\nrequirements:\n  \
                DockerRequirement:\n    dockerPull: debian:stable-slim\ninputs:\n  f:\n    \
                type: File\n    inputBinding: {position: 1}\noutputs:\n  out: stdout\n";
    let job = "f:\n  class: File\n  path: nothere.txt\n";
    let directory = folder(&[("tool.cwl", tool), ("job.yml", job)]);
    let (code, stdout) = check(directory.path(), &["tool.cwl", "-inputs", "job.yml"]);
    assert_eq!(code, 1, "{stdout}");
    let missing = directory.path().join("nothere.txt");
    assert_eq!(
        stdout,
        format!(
            "error: the input `f` names the file `{}`, which does not exist\ntool.cwl (CWL): 1 \
             error would stop a real run.\n",
            missing.display()
        )
    );
    let abstract_workflow = "cwlVersion: v1.2\nclass: Workflow\ninputs: []\noutputs: []\nsteps:\n  \
                op:\n    run: {class: Operation, inputs: [], outputs: {out: string?}}\n    in: []\n    \
                out: [out]\n";
    let directory = folder(&[("main.cwl", abstract_workflow)]);
    let (code, stdout) = check(directory.path(), &["main.cwl"]);
    assert_eq!(code, 1, "{stdout}");
    assert!(
        stdout.contains("error: step op runs an abstract Operation: Workflow has unrunnable"),
        "{stdout}"
    );
    let argo = "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {generateName: x-}\n\
                spec:\n  entrypoint: main\n  templates:\n  - name: main\n    resource:\n      \
                action: create\n      manifest: |\n        apiVersion: v1\n        kind: \
                ConfigMap\n        metadata: {name: x}\n";
    let directory = folder(&[("workflow.yaml", argo)]);
    let (code, stdout) = check(directory.path(), &["workflow.yaml"]);
    assert_eq!(code, 33, "{stdout}");
    assert!(stdout.contains("(decision 18)"), "{stdout}");
}
