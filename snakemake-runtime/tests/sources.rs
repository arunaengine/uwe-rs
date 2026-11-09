//! Checks workflow source staging without Snakefiles or conflicts with job-owned files, and
//! the working folder of a project folder.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use workflow_graph::{Launch, Simulator, execute};

/// As Snakemake's test_github_issue4003: a shell command runs a module of a package that the
/// rule does not declare.
const SNAKEFILE: &str = r#"
configfile: "config.yaml"
include: "rules/table.smk"

rule all:
    input: "default_name.out", "results/table.txt"

rule module:
    output: "default_name.out"
    shell: "python -m some_package.some_module > {output}"
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
fn stages_workflow_sources() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let workdir = folder.path().join("workflow");
    write(&workdir, "Snakefile", SNAKEFILE);
    write(
        &workdir,
        "rules/table.smk",
        "rule table:\n    output: 'results/table.txt'\n    conda: '../envs/tools.yaml'\n    script: '../scripts/table.py'\n",
    );
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
    let table_task = (graph.nodes.iter())
        .find(|node| node.task.step == "table")
        .expect("the table task");
    let environment = format!("conda-env:{}", workdir.join("envs/tools.yaml").display());
    assert_eq!(table_task.task.image.as_deref(), Some(environment.as_str()));
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

/// As `--directory .test` of Snakemake, a project folder works in its test folder, in a copy of
/// it in the launch folder, and writes nothing into the project.
#[test]
fn works_in_directories() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let project = folder.path().join("project");
    let snakefile = "configfile: 'config/config.yaml'\nrule all:\n    input: 'results/out.txt'\n\
                     rule copy:\n    input: config['data']\n    output: 'results/out.txt'\n    \
                     shell: 'cp {input} {output}'\n";
    write(&project, "workflow/Snakefile", snakefile);
    write(&project, ".test/config/config.yaml", "data: data/in.txt\n");
    write(&project, ".test/data/in.txt", "reads\n");
    let launch = Launch {
        target: project.clone(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let started = folder.path().join("launch");
    let options = snakemake_runtime::RunSettings {
        image: Some("debian:stable-slim".into()),
        workdir: Some(started.clone()),
        directory: Some(".test".into()),
        ..snakemake_runtime::RunSettings::default()
    };
    let settings = snakemake_runtime::Settings::default();
    let mut driver =
        snakemake_runtime::driver_with_options(&launch, settings, options).expect("a driver");
    let tasks = driver.start().expect("plans the run");
    let input = (tasks.iter().flat_map(|task| &task.inputs))
        .find(|input| input.path == "data/in.txt")
        .expect("the data file");
    assert_eq!(Path::new(&input.source), started.join("data/in.txt"));
    let mut kept: Vec<_> = walkdir(&project.join(".test"));
    kept.sort();
    assert_eq!(kept, ["config/config.yaml", "data/in.txt"]);
}

/// The files below `folder`, relative to it.
fn walkdir(folder: &Path) -> Vec<String> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(folder)
        .expect("lists the folder")
        .flatten()
    {
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name().to_string_lossy().into_owned();
            files.extend(
                walkdir(&path)
                    .into_iter()
                    .map(|file| format!("{name}/{file}")),
            );
        } else {
            files.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    files
}

/// The copy of a working folder neither reads nor writes outside: a link of the project that
/// leads outside or loops, and a link in the launch folder, stop the run before it starts.
#[test]
fn refuses_outer_links() {
    let snakefile = "rule all:\n    input: 'data/in.txt'\n";
    let planned = |root: &Path| {
        let launch = Launch {
            target: root.join("project"),
            scratch: root.join("scratch"),
            ..Launch::default()
        };
        let options = snakemake_runtime::RunSettings {
            workdir: Some(root.join("launch")),
            directory: Some(".test".into()),
            ..snakemake_runtime::RunSettings::default()
        };
        let settings = snakemake_runtime::Settings::default();
        match snakemake_runtime::driver_with_options(&launch, settings, options) {
            Ok(mut driver) => driver.start().is_ok(),
            Err(_) => false,
        }
    };
    let link = |target: &str, link: &Path| std::os::unix::fs::symlink(target, link).unwrap();
    for case in ["folder", "inner", "launch", "loop"] {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let root = folder.path();
        write(&root.join("project"), "workflow/Snakefile", snakefile);
        write(root, "outside/in.txt", "private\n");
        match case {
            "folder" => link("../outside", &root.join("project/.test")),
            "inner" => {
                write(&root.join("project"), ".test/data/in.txt", "reads\n");
                link("../../outside", &root.join("project/.test/inner"));
            }
            "launch" => {
                write(&root.join("project"), ".test/data/in.txt", "reads\n");
                std::fs::create_dir_all(root.join("launch")).unwrap();
                link("../outside", &root.join("launch/data"));
            }
            _ => {
                write(&root.join("project"), ".test/data/in.txt", "reads\n");
                link(".", &root.join("project/.test/loop"));
            }
        }
        assert!(!planned(root), "{case}");
        let outside = std::fs::read_dir(root.join("outside")).unwrap().count();
        assert_eq!(outside, 1, "{case}: nothing is written outside");
    }
}
