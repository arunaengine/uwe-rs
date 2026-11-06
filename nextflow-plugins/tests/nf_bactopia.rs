//! nf-bactopia against the plugin 2.1.7 with Nextflow 26.04.6: the probe scripts of
//! `fixtures/nf_bactopia` with the arguments of the recorded runs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::value::Value;
use nextflow_runtime::{Loader, Options, Run};

struct Console {
    text: String,
    launch: String,
}

impl Host for Console {
    fn print(&mut self, text: &str) {
        self.text.push_str(text);
    }

    fn launch_dir(&self) -> String {
        self.launch.clone()
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nf_bactopia")
}

/// Copies the probe folder, then runs a script with the params of the command line as text.
fn run(
    script: &str,
    arguments: &[(&str, &str)],
    recorded: &str,
) -> (String, String, Result<(), Error>) {
    let folder = tempfile::tempdir().expect("a folder");
    let launch = folder.path();
    std::fs::create_dir(launch.join("data")).expect("creates");
    for entry in std::fs::read_dir(fixtures().join("data")).expect("lists") {
        let path = entry.expect("an entry").path();
        std::fs::copy(
            &path,
            launch.join("data").join(path.file_name().expect("a name")),
        )
        .expect("copies");
    }
    for name in ["nextflow.config", "nextflow_schema.json"] {
        std::fs::copy(fixtures().join(name), launch.join(name)).expect("copies");
    }
    std::fs::copy(fixtures().join(script), launch.join("main.nf")).expect("copies");
    let mut console = Console {
        text: String::new(),
        launch: launch.to_string_lossy().into_owned(),
    };
    let params = arguments
        .iter()
        .map(|(name, value)| (Value::string(*name), Value::string(*value)));
    let settings = nextflow_config::Options {
        profiles: None,
        params: Value::map(params.collect::<Vec<_>>()),
        project_dir: console.launch.clone(),
        launch_dir: console.launch.clone(),
        work_dir: launch.join("work").to_string_lossy().into_owned(),
    };
    let file = launch.join("nextflow.config");
    let config =
        nextflow_config::load(std::slice::from_ref(&file), &settings, &mut console).expect("loads");
    let field = |label: &str| {
        (recorded.lines())
            .find_map(|line| line.strip_prefix(label))
            .map(|value| value.trim_start_matches([' ', ':']).trim().to_owned())
            .unwrap_or_default()
    };
    let options = Options {
        config,
        project_dir: console.launch.clone(),
        script_file: launch.join("main.nf").to_string_lossy().into_owned(),
        run_name: field("  runName"),
        config_files: vec![file.to_string_lossy().into_owned()],
        command_line: field("Command Line"),
        cli_params: arguments.iter().map(|(name, _)| name.to_string()).collect(),
        ..Options::default()
    };
    let module = Loader::default()
        .load(&launch.join("main.nf"))
        .expect("loads");
    let result = Run::start(&module, &options, &mut console).and_then(|(mut run, requests)| {
        assert!(requests.is_empty(), "the probes run no tasks");
        run.finish(&mut console)
    });
    let launch = console.launch.clone();
    (console.text, launch, result)
}

/// The lines of a run without Nextflow's own, with times masked and channel lines sorted.
fn normalized(text: &str, launch: &str) -> Vec<String> {
    let user = std::env::var("USER").unwrap_or_default();
    let mut lines: Vec<String> = Vec::new();
    for line in text.replace(launch, "<probe-dir>").lines() {
        if line.starts_with("[SUCCESS]") || line.starts_with("[FAILED]") {
            if lines.last().is_some_and(|last| last.is_empty()) {
                lines.pop();
            }
            continue;
        }
        if ["[PIPELINE]", "[WORKDIR]", "[exit code", "[ERROR]"]
            .iter()
            .any(|start| line.starts_with(start))
        {
            continue;
        }
        let line = match line.split_once(" : ") {
            Some((label @ ("Completed At    " | "Duration        "), _)) => {
                format!("{label} : <time>")
            }
            _ => line.to_owned(),
        };
        let line = match line.split_once(": ") {
            Some((label, name))
                if label.trim() == "userName" && (name == user || name == "<user>") =>
            {
                format!("{label}: <user>")
            }
            _ => line,
        };
        lines.push(line);
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    let channel = |line: &String| line.contains(" channel => ");
    let mut sorted: Vec<String> = lines.iter().filter(|line| channel(line)).cloned().collect();
    sorted.sort();
    let mut sorted = sorted.into_iter();
    lines
        .into_iter()
        .map(|line| {
            if channel(&line) {
                sorted.next().unwrap_or(line)
            } else {
                line
            }
        })
        .collect()
}

fn check(script: &str, arguments: &[(&str, &str)], recording: &str) {
    let recorded = std::fs::read_to_string(fixtures().join(recording)).expect("reads");
    let (text, launch, _) = run(script, arguments, &recorded);
    assert_eq!(
        normalized(&text, &launch),
        normalized(&recorded, "<probe-dir>"),
        "{recording}"
    );
}

#[test]
fn prints_run_summary() {
    check("prints.nf", &[], "prints-mono.out");
}

#[test]
fn prints_help() {
    check("prints.nf", &[("help", "true")], "help.out");
    check("prints.nf", &[("help_all", "true")], "help-all.out");
}

#[test]
fn matches_recorded_functions() {
    let arguments = [
        ("sample", "S1"),
        ("r1", "data/S1_R1.fastq.gz"),
        ("r2", "data/S1_R2.fastq.gz"),
        ("assembly", "data/notgz.fastq"),
    ];
    check("functions.nf", &arguments, "functions.out");
}

#[test]
fn reads_sample_inputs() {
    check(
        "inputs.nf",
        &[("samples", "data/fofn.tsv"), ("mode", "is_fofn")],
        "inputs-fofn.out",
    );
    let accessions = [
        ("accessions", "data/acc.tsv"),
        ("mode", "is_accessions"),
        ("genome_size", "7"),
    ];
    check("inputs.nf", &accessions, "inputs-accessions.out");
    let accession = [("accession", "GCF_000001.2"), ("mode", "is_accession")];
    check("inputs.nf", &accession, "inputs-accession.out");
}

#[test]
fn validates_parameters() {
    let bad = [
        ("sample", "S1"),
        ("r1", "data/notgz.fastq"),
        ("r2", "data/missing.fastq.gz"),
        ("genome_size", "abc"),
    ];
    check("inputs.nf", &bad, "inputs-badreads.out");
    let unknown = [
        ("sample", "S1"),
        ("r1", "data/S1_R1.fastq.gz"),
        ("r2", "data/S1_R2.fastq.gz"),
        ("outdir", "5"),
        ("unknown_param", "x"),
    ];
    check("inputs.nf", &unknown, "inputs-schemaerr.out");
    let conflict = [
        ("sample", "S1"),
        ("r1", "data/S1_R1.fastq.gz"),
        ("r2", "data/S1_R2.fastq.gz"),
        ("se", "data/S1_R1.fastq.gz"),
        ("max_downloads", "12"),
    ];
    // The test has no driver, which prints the summary of a run that fails at its start.
    let recorded = std::fs::read_to_string(fixtures().join("inputs-conflict.out")).expect("reads");
    let (text, launch, result) = run("inputs.nf", &conflict, &recorded);
    let (heard, expected) = (
        normalized(&text, &launch),
        normalized(&recorded, "<probe-dir>"),
    );
    assert_eq!(
        heard,
        expected[..heard.len()],
        "the banner of inputs-conflict.out"
    );
    let error = result.expect_err("the run fails").to_string();
    assert!(
        error.contains(
            "Cannot compare java.lang.String with value '12' and java.lang.Integer with value '10'"
        ),
        "{error}"
    );
}
