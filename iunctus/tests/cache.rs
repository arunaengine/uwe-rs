//! The global cache with real runs in Docker: a task that any run, pipeline or language asked
//! for before comes from the store, and two runs that ask for the same task at the same moment
//! run it once. The tests need Docker and the `debian:stable-slim` image; run them with
//! `cargo test -p iunctus --test cache -- --ignored --test-threads 1`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

mod common;

/// A module as nf-core writes them: one process, vendored into each pipeline that uses it.
const MODULE: &str = r#"
process WORDCOUNT {
    container 'debian:stable-slim'
    input:
    tuple val(id), path(text)
    output:
    tuple val(id), path("${id}.count"), emit: count
    script:
    """
    wc -w < ${text} > ${id}.count
    """
}
"#;

/// A pipeline that calls the module from its entry workflow.
const FIRST: &str = r#"
include { WORDCOUNT } from './modules/wordcount/main'

workflow {
    WORDCOUNT(channel.of(['sample', file('data/sample.txt')]))
    WORDCOUNT.out.count.view { id, count -> "${id}: ${count.text.trim()} words" }
}
"#;

/// Another pipeline: the module sits in a subworkflow of another name, and a step of its own
/// reads its result.
const SECOND: &str = r#"
include { WORDCOUNT } from './modules/wordcount/main'

workflow PREPARE {
    take:
    samples
    main:
    WORDCOUNT(samples)
    emit:
    counts = WORDCOUNT.out.count
}

process REPORT {
    container 'debian:stable-slim'
    input:
    tuple val(id), path(count)
    output:
    path('report.txt')
    script:
    """
    echo "${id} has \$(cat ${count}) words" > report.txt
    """
}

workflow {
    PREPARE(channel.of(['sample', file('data/sample.txt')]))
    REPORT(PREPARE.out.counts).view { report -> report.text.trim() }
}
"#;

/// Runs the binary in `directory` with the cache folder `cache`, and gives its exit code and
/// what it printed.
fn iunctus(directory: &Path, cache: &Path, arguments: &[&str]) -> (i32, String) {
    let output = common::command()
        .args(arguments)
        .current_dir(directory)
        .env("XDG_CACHE_HOME", cache)
        .output()
        .expect("runs iunctus");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    (
        output.status.code().unwrap_or(-1),
        format!("{stdout}{stderr}"),
    )
}

/// A pipeline in its own folder, with its own copy of the module and of the data.
fn pipeline(root: &Path, name: &str, script: &str) -> std::path::PathBuf {
    let folder = root.join(name);
    std::fs::create_dir_all(folder.join("modules/wordcount")).expect("makes the folders");
    std::fs::create_dir_all(folder.join("data")).expect("makes the folders");
    std::fs::write(folder.join("modules/wordcount/main.nf"), MODULE).expect("writes");
    std::fs::write(folder.join("data/sample.txt"), "one two three\n").expect("writes");
    std::fs::write(folder.join("main.nf"), script).expect("writes");
    folder
}

/// The text of each file below a folder.
fn delivered(folder: &Path) -> Vec<String> {
    let mut texts = Vec::new();
    for entry in std::fs::read_dir(folder)
        .expect("lists the folder")
        .flatten()
    {
        let path = entry.path();
        match path.is_dir() {
            true => texts.extend(delivered(&path)),
            false => texts.push(std::fs::read_to_string(path).expect("reads")),
        }
    }
    texts
}

/// The cache entries in the store of a cache folder.
fn entries(cache: &Path) -> usize {
    let folder = cache.join("iunctus/store/ac/v1/default");
    let entries = std::fs::read_dir(folder).map_or(Vec::new(), |entries| entries.collect());
    (entries.iter().flatten())
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".json"))
        .count()
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn a_module_runs_once_for_two_pipelines() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let cache = root.path().join("cache");
    let first = pipeline(root.path(), "first", FIRST);
    let second = pipeline(root.path(), "second", SECOND);
    let arguments = ["run", "main.nf", "-executor", "local"];
    let (code, output) = iunctus(&first, &cache, &arguments);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("[done] WORDCOUNT (1)"), "{output}");
    assert!(output.contains("sample: 3 words"), "{output}");
    // The other pipeline asks for the same task: the same image, command, inputs by content
    // and environment. Its process has another name and sits elsewhere; the task is the same.
    let (code, output) = iunctus(&second, &cache, &arguments);
    assert_eq!(code, 0, "{output}");
    assert!(
        output.contains("[cached] PREPARE:WORDCOUNT (1)"),
        "{output}"
    );
    assert!(output.contains("[done] REPORT (1)"), "{output}");
    assert!(output.contains("sample has 3 words"), "{output}");
    assert_eq!(entries(&cache), 2);
}

/// A CWL tool that counts the words of a file.
const TOOL: &str = r#"
cwlVersion: v1.2
class: CommandLineTool
requirements:
  DockerRequirement:
    dockerPull: debian:stable-slim
baseCommand: [bash, -c]
arguments:
  - wc -w < $(inputs.text.path) > count.txt
inputs:
  text: File
outputs:
  count:
    type: File
    outputBinding:
      glob: count.txt
"#;

/// An Argo workflow whose template describes the same task as the CWL tool: the same image,
/// command, working directory, environment and input by content, as the CWL standard sets
/// them.
const TEMPLATE: &str = r#"
apiVersion: argoproj.io/v1alpha1
kind: Workflow
metadata:
  generateName: count-
spec:
  entrypoint: count
  templates:
    - name: count
      inputs:
        artifacts:
          - name: text
            path: /var/lib/cwl/inputs/0/sample.txt
            raw:
              data: |
                one two three
      container:
        image: debian:stable-slim
        workingDir: /var/spool/cwl
        env:
          - name: HOME
            value: /var/spool/cwl
          - name: TMPDIR
            value: /tmp
        command: [bash, -c]
        args: ["wc -w < /var/lib/cwl/inputs/0/sample.txt > count.txt"]
      outputs:
        artifacts:
          - name: count
            path: count.txt
"#;

/// A Snakemake rule that does the same work as the CWL tool, as Snakemake describes it.
const RULE: &str = r#"
rule count:
    input: "sample.txt"
    output: "count.txt"
    container: "docker://debian:stable-slim"
    shell: "wc -w < {input} > {output}"
"#;

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn languages_share_a_task_where_their_descriptions_are_equal() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let cache = root.path().join("cache");
    let folder = root.path();
    std::fs::write(folder.join("sample.txt"), "one two three\n").expect("writes");
    std::fs::write(folder.join("count.cwl"), TOOL).expect("writes");
    std::fs::write(
        folder.join("job.yml"),
        "text:\n  class: File\n  path: sample.txt\n",
    )
    .expect("writes");
    std::fs::write(folder.join("count.yaml"), TEMPLATE).expect("writes");
    std::fs::write(folder.join("Snakefile"), RULE).expect("writes");
    let cwl = [
        "run",
        "count.cwl",
        "-inputs",
        "job.yml",
        "-executor",
        "local",
    ];
    let output_dir = folder.join("argo").to_string_lossy().into_owned();
    let argo = [
        "run",
        "count.yaml",
        "-executor",
        "local",
        "-output-dir",
        &output_dir,
    ];
    let (code, output) = iunctus(folder, &cache, &cwl);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("[done] count"), "{output}");
    // The Argo template describes the same task, so it comes from the cache.
    let (code, output) = iunctus(folder, &cache, &argo);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("[cached] count"), "{output}");
    // The artifact was delivered from the stored result of the CWL tool.
    assert_eq!(delivered(&folder.join("argo")), ["3\n"]);
    // The Snakemake rule does the same work, but Snakemake describes its jobs in its own way:
    // another command, no fixed working directory, other variables. So it runs.
    std::fs::remove_file(folder.join("count.txt")).expect("removes the CWL output");
    let (code, output) = iunctus(folder, &cache, &["run", "Snakefile", "-executor", "local"]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("[done] count"), "{output}");
    assert_eq!(entries(&cache), 2);
}

/// A pipeline whose one task takes a few seconds.
const SLOW: &str = r#"
process SLOW {
    container 'debian:stable-slim'
    output:
    path('done.txt')
    script:
    """
    sleep 4
    echo done > done.txt
    """
}

workflow {
    SLOW()
}
"#;

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn two_runs_at_the_same_moment_run_a_task_once() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let cache = root.path().join("cache");
    let folders: Vec<_> = ["one", "two"]
        .iter()
        .map(|name| {
            let folder = root.path().join(name);
            std::fs::create_dir_all(&folder).expect("makes the folder");
            std::fs::write(folder.join("main.nf"), SLOW).expect("writes");
            folder
        })
        .collect();
    // Both runs start at once, on the same store.
    let started: Vec<_> = folders
        .iter()
        .map(|folder| {
            common::command()
                .args(["run", "main.nf", "-executor", "local"])
                .current_dir(folder)
                .env("XDG_CACHE_HOME", &cache)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("runs iunctus")
        })
        .collect();
    let mut lines = Vec::new();
    for child in started {
        let output = child.wait_with_output().expect("iunctus ends");
        assert!(output.status.success(), "{output:?}");
        lines.push(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    // One run ran the task; the other waited for it instead of running it too.
    lines.sort();
    assert_eq!(lines, ["[cached] SLOW\n", "[done] SLOW\n"], "{lines:?}");
    assert_eq!(entries(&cache), 1);
}
