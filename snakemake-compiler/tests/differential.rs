//! Compiles Snakemake workflows into wir, runs each natively and compiled through the wir
//! runtime on the simulator, and compares their tasks, acceptance and target files.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value as Json, json};
use snakemake_runtime::{RunSettings, Settings};
use wir::{Document, Kind};
use workflow_graph::{
    Driver, ExecutionGraph, Launch, Order, RunOptions, Simulator, Task, TaskId, execute_with,
};

/// The counts of `just snakemake-test-workflows`: native runs, how workflows compile, and
/// compiled runs that succeed, end as the native run, and give its tasks and target files.
const RECORDED: Counts = Counts {
    workflows: 890,
    native: 633,
    full: 616,
    partly: 49,
    failed: 224,
    compiled: 594,
    ends: 616,
    same: 594,
    equal: 594,
};

#[derive(Debug, Default, PartialEq)]
struct Counts {
    workflows: usize,
    native: usize,
    full: usize,
    partly: usize,
    failed: usize,
    compiled: usize,
    ends: usize,
    same: usize,
    equal: usize,
}

const LAUNCHES: &str =
    include_str!("../../snakemake-runtime/tests/fixtures/run_test_workflows_launches.txt");

const SNAKEFILE: &str = r#"
SAMPLES = ["a", "b"]

rule all:
    input: expand("counts/{s}.txt", s=SAMPLES), "summary.txt"

rule make:
    output: temp("raw/{s}.txt")
    params: word=lambda wildcards: wildcards.s.upper()
    shell: "echo {params.word} > {output}"

rule count:
    input: "raw/{s}.txt"
    output: "counts/{s}.txt"
    log: "logs/{s}.log"
    retries: 1
    shell: "wc -c {input} > {output} 2> {log}"

rule summary:
    input: expand("counts/{s}.txt", s=SAMPLES)
    output: protected("summary.txt")
    threads: 2
    resources: mem_mb=100
    run:
        with open(output[0], "w") as handle:
            handle.write(str(len(input)))
"#;

/// The tasks of a simulated run as text with acceptance, cache key and resume fingerprint,
/// sorted; folders and task numbers are left out.
type Ran = Vec<(String, Option<bool>, String, String)>;

fn simulate(driver: &mut dyn Driver, folders: &[&Path], simulator: &Path) -> Result<Ran, String> {
    let mut executor = Simulator::new(simulator);
    let options = RunOptions {
        max_pending: 8,
        order: Order::AsAsked,
    };
    let execution = execute_with("differential", driver, &mut executor, &options);
    let graph = execution.into_result().map_err(|error| error.to_string())?;
    Ok(described(&graph, folders, simulator))
}

/// Text with the folders of the run and the numbers of the tasks in simulator paths left out.
fn normal(text: &str, folders: &[&Path], simulator: &Path, names: &HashMap<u64, String>) -> String {
    let mut text = text.replace(&simulator.to_string_lossy()[..], "<simulator>");
    for folder in folders {
        text = text.replace(&folder.to_string_lossy()[..], "<work>");
    }
    let marker = "<simulator>/work/";
    let mut shown = String::new();
    while let Some(start) = text.find(marker) {
        let after = start + marker.len();
        let digits = text[after..]
            .chars()
            .take_while(char::is_ascii_digit)
            .count();
        let number = text[after..after + digits].parse::<u64>().ok();
        let name = number.and_then(|number| names.get(&number)).cloned();
        shown.push_str(&text[..after]);
        shown.push_str(&name.unwrap_or_default().replace('"', "'"));
        text = text[after + digits..].to_owned();
    }
    shown + &text
}

fn described(
    graph: &ExecutionGraph,
    folders: &[&Path],
    simulator: &Path,
) -> Vec<(String, Option<bool>, String, String)> {
    let names: HashMap<u64, String> = (graph.nodes.iter())
        .map(|node| (node.task.id.0, node.task.name.clone()))
        .collect();
    let mut tasks: Vec<_> = (graph.nodes.iter())
        .map(|node| {
            let task = Task {
                id: TaskId(0),
                after: Vec::new(),
                ..node.task.clone()
            };
            let text = serde_json::to_string(&task).expect("serializes");
            let text = normal(&text, folders, simulator, &names);
            let task: Task = serde_json::from_str(&text).expect("parses");
            let request = task_runner::request(&task, task.image.clone(), Vec::new(), None);
            // A rule without a directive, like `all`, runs no program and has no key.
            let key = request.spec.cache_key().map(|key| key.to_string());
            let key = key.unwrap_or_else(|error| format!("{error:?}"));
            let fingerprint = run_database::fingerprint(&task, &[]);
            (text, node.accepted, key, fingerprint)
        })
        .collect();
    tasks.sort_by(|one, other| one.3.cmp(&other.3));
    tasks
}

/// How one workflow compared, and which counts it adds to.
#[derive(Debug, Default)]
struct Compared {
    native: bool,
    full: bool,
    partly: bool,
    failed: bool,
    compiled: bool,
    ends: bool,
    same: bool,
    equal: bool,
    shown: String,
}

/// Runs a workflow natively, compiles it and runs the compiled document, with the files of
/// the runs below `scratch`.
fn compare(launch: &Launch, workdir: &Path, scratch: &Path) -> Compared {
    let mut compared = Compared::default();
    let run = RunSettings {
        workdir: Some(workdir.to_owned()),
        ..RunSettings::default()
    };
    let (own, compiled_scratch) = (scratch.join("native"), scratch.join("compiled"));
    let native_launch = Launch {
        scratch: own.clone(),
        ..launch.clone()
    };
    let folders = [own.as_path(), compiled_scratch.as_path(), workdir, scratch];
    let native = snakemake_runtime::spawn_driver(&native_launch, Settings::default(), run.clone());
    let native = native
        .map_err(|error| error.message)
        .and_then(|mut driver| {
            let ran = simulate(&mut driver, &folders, &scratch.join("simulator-native"))?;
            Ok((ran, driver.exported().unwrap_or_default()))
        });
    compared.native = native.is_ok();
    let compile_launch = Launch {
        scratch: scratch.join("compile"),
        ..launch.clone()
    };
    // Sources keep absolute locations, so the document runs from the scratch folder.
    let compiled =
        snakemake_compiler::compile(&compile_launch, Settings::default(), run.clone(), scratch);
    let compiled = match compiled {
        Ok(compiled) => compiled,
        Err(error) => {
            compared.failed = true;
            compared.shown = format!("does not compile: {}", first_line(&error));
            return compared;
        }
    };
    if let Some(kept) = compiled.kept.first() {
        compared.partly = true;
        compared.shown = format!("stays native: {}", kept.reason);
        return compared;
    }
    compared.full = true;
    if let Err(problems) = wir::verify(&compiled.document) {
        compared.shown = format!("does not verify: {:?}", problems.first());
        return compared;
    }
    let ran = run_compiled(&compiled.document, run, &compiled_scratch).and_then(|mut driver| {
        simulate(&mut *driver, &folders, &scratch.join("simulator-compiled"))
    });
    compared.compiled = ran.is_ok();
    compared.shown = match (&native, &ran) {
        (Err(error), Err(_)) => {
            compared.ends = true;
            format!("both runs fail: {}", first_line(error))
        }
        (Err(error), Ok(_)) => format!("only the native run fails: {}", first_line(error)),
        (Ok(_), Err(error)) => format!("the compiled run fails: {}", first_line(error)),
        (Ok((native, targets)), Ok(ran)) => {
            compared.same = native == ran;
            compared.equal = *targets == named_targets(&compiled.document);
            // `SNAKEMAKE_COMPARE=2` prints the first pair of tasks that differ.
            let pairs = native.iter().zip(ran);
            if let Some(pair) = pairs.into_iter().find(|(one, other)| one != other)
                && std::env::var("SNAKEMAKE_COMPARE").is_ok_and(|value| value == "2")
            {
                eprintln!("native:   {:?}\ncompiled: {:?}", pair.0, pair.1);
            }
            compared.ends = compared.same;
            match (compared.same, compared.equal) {
                (true, true) => format!("same: {} tasks", native.len()),
                (true, false) => "other target files".to_owned(),
                _ => format!(
                    "other tasks: {} native, {} compiled",
                    native.len(),
                    ran.len()
                ),
            }
        }
    };
    compared
}

/// The target files that the outputs of the entry graph of `document` name, as values.
fn named_targets(document: &Document) -> Json {
    let Some(graph) = document
        .graphs
        .iter()
        .find(|graph| graph.name == document.entry)
    else {
        return Json::Null;
    };
    let file = |location: &str, class: &str| {
        let trimmed = location.trim_end_matches('/');
        let basename = trimmed.rsplit('/').next().unwrap_or(trimmed);
        json!({"class": class, "location": location, "basename": basename})
    };
    let mut targets = serde_json::Map::new();
    for op in &graph.ops {
        let (Kind::Expr(expr), Some(name)) = (&op.kind, op.path.strip_prefix("result.")) else {
            continue;
        };
        let class = match serde_json::to_string(&op.outputs[0].r#type) {
            Ok(text) if text.contains("directory") => "Directory",
            _ => "File",
        };
        // A list of files and folders names each in a typed declaration.
        let declared: Vec<Json> = (expr.library.iter())
            .filter_map(|line| {
                let (declaration, text) = line.split_once(" = ")?;
                let class = declaration.split(' ').next()?;
                Some(file(&serde_json::from_str::<String>(text).ok()?, class))
            })
            .collect();
        if !declared.is_empty() {
            targets.insert(name.to_owned(), Json::Array(declared));
            continue;
        }
        let value = match serde_json::from_str(&expr.code) {
            Ok(Json::String(location)) => file(&location, class),
            Ok(Json::Array(items)) => (items.iter())
                .map(|item| file(item.as_str().unwrap_or_default(), class))
                .collect(),
            _ => Json::Null,
        };
        targets.insert(name.to_owned(), value);
    }
    Json::Object(targets)
}

/// The wir runtime for `document`, written below `scratch`, with the Snakemake adapter and
/// the run settings `run`.
fn run_compiled(
    document: &Document,
    run: RunSettings,
    scratch: &Path,
) -> Result<Box<dyn Driver>, String> {
    std::fs::create_dir_all(scratch).map_err(|error| error.to_string())?;
    let path = scratch.join("compiled.wir.json");
    let text = serde_json::to_string(document).map_err(|error| error.to_string())?;
    std::fs::write(&path, text).map_err(|error| error.to_string())?;
    let launch = Launch {
        target: path,
        scratch: scratch.to_owned(),
        simulated: true,
        ..Launch::default()
    };
    let mut settings = wir_runtime::Settings {
        snakemake: wir_runtime::snakemake::Options {
            settings: Settings::default(),
            run,
        },
        ..wir_runtime::Settings::default()
    };
    wir_runtime::snakemake::register(&mut settings.adapters, settings.snakemake.clone());
    wir_runtime::driver(&launch, settings).map_err(|error| error.message)
}

fn first_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect()
}

/// Compares the runs of `snakefile` with the targets of the command line.
fn compared(snakefile: &str, targets: &[&str]) -> Compared {
    compared_with(snakefile, targets, &[])
}

/// Compares the runs of `snakefile` with `files` beside it.
fn compared_with(snakefile: &str, targets: &[&str], files: &[(&str, &str)]) -> Compared {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let root = folder.path().join("work");
    std::fs::create_dir(&root).expect("makes the working folder");
    std::fs::write(root.join("Snakefile"), snakefile).expect("writes the Snakefile");
    for (name, text) in files {
        std::fs::write(root.join(name), text).expect("writes a file");
    }
    let launch = Launch {
        target: root.join("Snakefile"),
        targets: targets.iter().map(|target| (*target).to_owned()).collect(),
        simulated: true,
        ..Launch::default()
    };
    compare(&launch, &root, &folder.path().join("scratch"))
}

#[test]
fn compiled_runs_match() {
    let compared = compared(SNAKEFILE, &[]);
    assert_eq!(compared.shown, "same: 6 tasks");
}

/// A group job, a pipe and a service with its users each run as one task.
#[test]
fn compiled_groups_match() {
    let snakefile = SNAKEFILE.replace(
        "    shell: \"echo {params.word} > {output}\"",
        "    group: \"g\"\n    shell: \"echo {params.word} > {output}\"",
    );
    let snakefile = snakefile.replace("    retries: 1", "    retries: 1\n    group: \"g\"");
    let piped = "rule all:\n    input: \"b.txt\"\n\nrule a:\n    output: pipe(\"a.pipe\")\n    shell: \"echo a > {output}\"\n\nrule b:\n    input: \"a.pipe\"\n    output: \"b.txt\"\n    shell: \"cat {input} > {output}\"\n";
    let served = "rule all:\n    input: \"b.txt\", \"c.txt\"\n\nrule a:\n    output: service(\"a.socket\")\n    shell: \"sleep 100 > {output}\"\n\nrule b:\n    input: \"a.socket\"\n    output: \"b.txt\"\n    shell: \"head -n1 {input} > {output}\"\n\nrule c:\n    input: \"a.socket\"\n    output: \"c.txt\"\n    shell: \"head -n1 {input} > {output}\"\n";
    // The default target `all` is in a group whose task another rule names.
    let grouped_all = "rule all:\n    group: \"g\"\n    input: \"a.txt\", \"b.txt\"\n\nrule a:\n    output: \"a.txt\"\n    group: \"g\"\n    shell: \"touch {output}\"\n\nrule b:\n    output: \"b.txt\"\n    group: \"g\"\n    shell: \"touch {output}\"\n";
    let cases = [
        (snakefile.as_str(), "same: 4 tasks"),
        (piped, "same: 2 tasks"),
        (served, "same: 2 tasks"),
        (grouped_all, "same: 1 tasks"),
    ];
    for (snakefile, shown) in cases {
        let compared = compared(snakefile, &[]);
        assert_eq!(compared.shown, shown);
    }
}

/// Target files and folders in one list keep their classes.
#[test]
fn compiled_targets_match() {
    let snakefile = "rule all:\n    input: \"a\", \"b.txt\"\n\nrule a:\n    output: directory(\"a\")\n    shell: \"mkdir {output}\"\n\nrule b:\n    output: \"b.txt\"\n    shell: \"touch {output}\"\n";
    assert_eq!(compared(snakefile, &[]).shown, "same: 3 tasks");
}

/// A workflow with a checkpoint runs as a region that the Snakemake planner plans again
/// once the checkpoint ended, as natively.
#[test]
fn checkpoints_match() {
    let snakefile = "import os\n\nrule all:\n    input: \"aggregated.txt\"\n\ncheckpoint split:\n    output: directory(\"parts\")\n    shell: \"mkdir -p {output}\"\n\nrule process:\n    input: \"parts/{n}.txt\"\n    output: \"processed/{n}.txt\"\n    shell: \"cp {input} {output}\"\n\ndef parts(wildcards):\n    folder = checkpoints.split.get().output[0]\n    names = glob_wildcards(os.path.join(folder, \"{n}.txt\")).n\n    return expand(\"processed/{n}.txt\", n=names)\n\nrule aggregate:\n    input: parts\n    output: \"aggregated.txt\"\n    shell: \"cat {input} > {output}\"\n";
    let compared = compared(snakefile, &[]);
    assert!(compared.full, "{}", compared.shown);
    assert!(compared.same, "{}", compared.shown);
}

/// The native run plans again after each checkpoint, also one that no input function reads,
/// and a checkpoint whose files are there runs no task and plans no more.
#[test]
fn replanned_checkpoints_match() {
    let plain = "rule all:\n    input: \"d.txt\"\n\ncheckpoint c:\n    output: \"c.txt\"\n    shell: \"touch {output}\"\n\nrule d:\n    input: \"c.txt\"\n    output: \"d.txt\"\n    shell: \"cp {input} {output}\"\n";
    let current = "rule all:\n    input: \"t.txt\"\n\ncheckpoint c:\n    output: \"c.txt\"\n    shell: \"touch {output}\"\n\ndef made(wildcards):\n    return checkpoints.c.get().output[0]\n\nrule p:\n    input: made\n    output: \"p.txt\"\n    shell: \"cp {input} {output}\"\n\nrule t:\n    input: \"p.txt\"\n    output: \"t.txt\"\n    shell: \"cp {input} {output}\"\n";
    let made = [("c.txt", "c\n"), ("p.txt", "c\n")];
    for (snakefile, files) in [(plain, &[][..]), (current, &made[..])] {
        let compared = compared_with(snakefile, &[], files);
        assert!(compared.full && compared.same, "{}", compared.shown);
    }
}

/// A `cwl:` job runs the tasks of its CWL run, as natively.
#[test]
fn cwl_jobs_match() {
    let tool = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: sort\ninputs:\n  input:\n    type: File\n    inputBinding: {position: 1}\nstdout: sorted.txt\noutputs:\n  output:\n    type: stdout\n";
    let snakefile = "rule all:\n    input: \"sorted.txt\"\n\nrule make:\n    output: \"test.txt\"\n    shell: \"echo b > {output}\"\n\nrule a:\n    input: input=\"test.txt\"\n    output: output=\"sorted.txt\"\n    cwl: \"tool.cwl\"\n";
    let compared = compared_with(snakefile, &[], &[("tool.cwl", tool)]);
    assert!(compared.full, "{}", compared.shown);
    assert_eq!(compared.shown, "same: 3 tasks");
}

/// Commands of workflow code run while the run plans and while a job is prepared, for two
/// targets.
#[test]
fn compiled_commands_match() {
    let snakefile = SNAKEFILE.replace("rule all:", "shell(\"echo planned\")\n\nrule all:");
    let snakefile = snakefile.replace(
        "    threads: 2",
        "    threads: 2\n    conda: lambda wildcards: shell(\"echo env.yaml\", read=True).strip()",
    );
    let compared = compared(&snakefile, &["summary.txt", "counts/a.txt"]);
    assert_eq!(compared.shown, "same: 7 tasks");
}

/// Compiles `snakefile` in a folder with the config file `config`, writes `changed` there,
/// and runs the compiled document; its tasks, or why it failed.
fn rerun(snakefile: &str, config: &str, changed: &[(&str, &str)]) -> Result<Ran, String> {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let root = folder.path().join("work");
    std::fs::create_dir(&root).expect("makes the working folder");
    std::fs::write(root.join("Snakefile"), snakefile).expect("writes the Snakefile");
    std::fs::write(root.join("config.yaml"), config).expect("writes the config");
    let launch = Launch {
        target: root.join("Snakefile"),
        configs: vec![root.join("config.yaml")],
        scratch: folder.path().join("compile"),
        simulated: true,
        ..Launch::default()
    };
    let run = RunSettings {
        workdir: Some(root.clone()),
        ..RunSettings::default()
    };
    let scratch = folder.path().join("scratch");
    let compiled =
        snakemake_compiler::compile(&launch, Settings::default(), run.clone(), &scratch)?;
    for (name, text) in changed {
        std::fs::write(root.join(name), text).expect("writes a file");
    }
    let mut driver = run_compiled(&compiled.document, run, &scratch.join("compiled"))?;
    simulate(&mut *driver, &[&root], &scratch.join("simulator"))
}

/// A run whose plan reads the inputs of a job in another order refuses the job.
#[test]
fn refuses_other_layouts() {
    let snakefile = "rule c:\n    input: expand(\"{n}.txt\", n=config[\"order\"])\n    output: \"c.txt\"\n    shell: \"cat {input} > {output}\"\n\nrule make:\n    output: \"{n}.txt\"\n    wildcard_constraints: n=\"[xy]\"\n    shell: \"echo {wildcards.n} > {output}\"\n";
    let swapped = [("config.yaml", "order: [y, x]\n")];
    let ran = rerun(snakefile, "order: [x, y]\n", &swapped);
    let error = ran.expect_err("refuses the job");
    assert!(
        error.contains("plans no job of rule c as compiled"),
        "{error}"
    );
    assert!(rerun(snakefile, "order: [x, y]\n", &[]).is_ok());
}

/// A compiled group whose files are current skips its jobs, which a run plans one by one.
#[test]
fn skips_current_groups() {
    let snakefile = "rule all:\n    input: \"b.txt\"\n\nrule a:\n    output: \"a.txt\"\n    group: \"g\"\n    shell: \"touch {output}\"\n\nrule b:\n    input: \"a.txt\"\n    output: \"b.txt\"\n    group: \"g\"\n    shell: \"cp {input} {output}\"\n";
    let made = [("a.txt", "a\n"), ("b.txt", "a\n")];
    let ran = rerun(snakefile, "{}\n", &made).expect("skips the group");
    assert_eq!(ran, Ran::new(), "nothing needs to run, as natively");
}

/// How a test workflow is launched: its working folder and Snakefile, config files, targets
/// and profiles, from the launches of the test-workflows test of snakemake-runtime.
fn launch_of(name: &str, folder: &Path, scratch: &Path) -> (Launch, PathBuf) {
    let mut launch = Launch {
        target: folder.to_owned(),
        scratch: scratch.to_owned(),
        simulated: true,
        ..Launch::default()
    };
    let mut workdir = folder.to_owned();
    let line = (LAUNCHES.lines())
        .filter(|line| !line.starts_with('#'))
        .find(|line| line.split('\t').next() == Some(name));
    for field in line.into_iter().flat_map(|line| line.split('\t').skip(2)) {
        let (key, value) = field.split_once('=').unwrap_or((field, ""));
        match key {
            "directory" => workdir = folder.join(value),
            "snakefile" => launch.target = folder.join(value),
            "configfile" => launch.configs.push(folder.join(value)),
            "config" => {
                let (name, value) = value.split_once('=').expect("a config value");
                launch.params.push((name.to_owned(), value.to_owned()));
            }
            "target" => launch.targets.push(value.to_owned()),
            "profile" | "workflow-profile" => {
                let path = folder.join(value);
                let profile = match path.exists() {
                    true => path.to_string_lossy().into_owned(),
                    false => value.to_owned(),
                };
                launch.profiles.push(profile);
            }
            _ => {}
        }
    }
    if launch.target == folder && workdir != folder {
        launch.target = workdir.clone();
    }
    (launch, workdir)
}

/// The workflow folders of the test workflows that have a Snakefile where Snakemake looks.
fn workflows(root: &Path) -> Vec<(String, PathBuf)> {
    let mut found = Vec::new();
    for (group, folder) in [
        ("catalog", root.join("catalog")),
        ("tests", root.join("snakemake-tests/tests")),
    ] {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && snakemake_runtime::find_snakefile(&path).is_some() {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                found.push((format!("{group}/{}", name.replace(' ', "_")), path));
            }
        }
    }
    found.sort();
    found
}

/// Compares one workflow when the test binary runs as a child for it, and writes the result.
#[test]
#[ignore = "a child of the test-workflows test"]
fn compare_one() {
    let (Some(name), Some(folder), Some(result), Some(scratch)) = (
        std::env::var_os("SNAKEMAKE_COMPARE_NAME"),
        std::env::var_os("SNAKEMAKE_COMPARE_FOLDER"),
        std::env::var_os("SNAKEMAKE_COMPARE_RESULT"),
        std::env::var_os("SNAKEMAKE_COMPARE_SCRATCH"),
    ) else {
        return;
    };
    let (folder, scratch) = (PathBuf::from(folder), PathBuf::from(scratch));
    let (launch, workdir) = launch_of(&name.to_string_lossy(), &folder, &scratch);
    let compared = compare(&launch, &workdir, &scratch);
    let flags = [
        compared.native,
        compared.full,
        compared.partly,
        compared.failed,
        compared.compiled,
        compared.ends,
        compared.same,
        compared.equal,
    ];
    let flags: Vec<&str> = flags
        .iter()
        .map(|flag| if *flag { "1" } else { "0" })
        .collect();
    let shown = compared.shown.replace('\n', " ");
    std::fs::write(result, format!("{}\t{shown}", flags.join(""))).expect("writes the result");
}

/// Compiles and runs every test workflow in a child process with a time limit and compares the
/// counts with the recorded ones; `SNAKEMAKE_COMPARE=1` prints the result of each.
#[test]
#[ignore = "needs the test workflows of just snakemake-test-workflows"]
fn matches_recorded() {
    let root = std::env::var_os("SNAKEMAKE_TEST_WORKFLOWS").map(PathBuf::from);
    let root = root.expect("SNAKEMAKE_TEST_WORKFLOWS names the test workflows");
    let results = tempfile::tempdir().expect("a temporary directory");
    let print = std::env::var_os("SNAKEMAKE_COMPARE").is_some();
    let mut counts = Counts::default();
    for (index, (name, folder)) in workflows(&root).into_iter().enumerate() {
        let result = results.path().join(index.to_string());
        // The parent removes the scratch folder, also after the time limit ended the child.
        let scratch = tempfile::tempdir().expect("a temporary directory");
        let status = Command::new("timeout")
            .arg("300")
            .arg(std::env::current_exe().expect("the test binary"))
            .args(["--exact", "compare_one", "--ignored", "--test-threads", "1"])
            .env("SNAKEMAKE_COMPARE_NAME", &name)
            .env("SNAKEMAKE_COMPARE_FOLDER", &folder)
            .env("SNAKEMAKE_COMPARE_RESULT", &result)
            .env("SNAKEMAKE_COMPARE_SCRATCH", scratch.path())
            .env("UWE_QUIET", "1")
            .env("LC_CTYPE", "C.UTF-8")
            .output()
            .expect("runs the test binary");
        let text = std::fs::read_to_string(&result).unwrap_or_else(|_| {
            let code = status
                .status
                .code()
                .map_or("a signal".into(), |code| code.to_string());
            format!("00000000\tthe child ended with {code}")
        });
        let (flags, shown) = text.split_once('\t').unwrap_or((&text, ""));
        let flag = |position: usize| flags.as_bytes().get(position) == Some(&b'1');
        counts.workflows += 1;
        for (position, count) in [
            &mut counts.native,
            &mut counts.full,
            &mut counts.partly,
            &mut counts.failed,
            &mut counts.compiled,
            &mut counts.ends,
            &mut counts.same,
            &mut counts.equal,
        ]
        .into_iter()
        .enumerate()
        {
            *count += usize::from(flag(position));
        }
        if print {
            println!("{name}\t{shown}");
        }
    }
    assert_eq!(counts, RECORDED);
}
