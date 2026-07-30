//! Every job sees the source files of its workflow at their paths in its work directory, as
//! Snakemake's jobs see them in the working directory: the files that git tracks, the folders
//! of scripts, conda files and config files, but not the Snakefiles, and not where the job's
//! own files go.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use workflow_graph::{Launch, Simulator, execute};

/// As Snakemake's test_github_issue4003: a shell command runs a module of a package that the
/// rule does not declare.
const SNAKEFILE: &str = r#"
configfile: "config.yaml"

rule all:
    input: "default_name.out", "results/table.txt"

rule module:
    output: "default_name.out"
    shell: "python -m some_package.some_module > {output}"

rule table:
    output: "results/table.txt"
    conda: "envs/tools.yaml"
    script: "scripts/table.py"
"#;

fn write(folder: &Path, path: &str, text: &str) {
    let file = folder.join(path);
    std::fs::create_dir_all(file.parent().expect("a parent")).expect("makes the folder");
    std::fs::write(file, text).expect("writes");
}

fn git(folder: &Path, arguments: &[&str]) {
    let output = std::process::Command::new("git")
        .args(arguments)
        .current_dir(folder)
        .output()
        .expect("runs git");
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn jobs_see_the_source_files_of_the_workflow() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let workdir = folder.path().join("workflow");
    write(&workdir, "Snakefile", SNAKEFILE);
    write(&workdir, "config.yaml", "samples: 2\n");
    write(&workdir, "some_package/__init__.py", "");
    write(&workdir, "some_package/some_module.py", "print('works')\n");
    write(&workdir, "some_package/helpers.py", "");
    write(&workdir, "results/.gitkeep", "");
    write(&workdir, "envs/tools.yaml", "dependencies: [python]\n");
    write(
        &workdir,
        "scripts/table.py",
        "open(snakemake.output[0], 'w')\n",
    );
    write(&workdir, "scripts/util/common.py", "");
    write(&workdir, "data.untracked", "");
    git(&workdir, &["init", "-q"]);
    git(
        &workdir,
        &["add", "Snakefile", "config.yaml", "some_package", "results"],
    );
    let launch = Launch {
        target: workdir.clone(),
        scratch: folder.path().join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let mut simulator = Simulator::new(folder.path().join("simulator"));
    let execution = execute("Snakefile", driver.as_mut(), &mut simulator);
    let graph = execution.into_result().expect("the run succeeds");
    let inputs = |step: &str| -> Vec<String> {
        let node = (graph.nodes.iter())
            .find(|node| node.task.step == step)
            .expect("a task of the rule");
        let mut paths: Vec<String> = (node.task.inputs.iter())
            .filter(|input| !input.path.starts_with('/') && !input.path.starts_with(".snakemake"))
            .map(|input| input.path.clone())
            .collect();
        paths.sort();
        paths
    };
    // Tracked files, the config file, the conda file and the folder of every script; not the
    // Snakefile, not untracked data, and not what lies where the job's outputs go.
    let everything = [
        "config.yaml",
        "envs/tools.yaml",
        "results/.gitkeep",
        "scripts/table.py",
        "scripts/util/common.py",
        "some_package/__init__.py",
        "some_package/helpers.py",
        "some_package/some_module.py",
    ];
    assert_eq!(inputs("module"), everything);
    let table: Vec<&str> = (everything.iter().copied())
        .filter(|path| !path.starts_with("results/"))
        .chain(["results"])
        .collect();
    let mut table: Vec<String> = table.into_iter().map(str::to_owned).collect();
    table.sort();
    assert_eq!(inputs("table"), table);
}
