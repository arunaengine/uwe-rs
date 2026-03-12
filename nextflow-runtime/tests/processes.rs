//! Runs workflows with processes against a fake executor that makes each expected output file.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::interpreter::Host;
use nextflow_evaluator::lower::lower_module;
use nextflow_evaluator::value::PathValue;
use nextflow_parser::parse_script;
use nextflow_runtime::{Options, Run, TaskOutcome, TaskRequest};

const GUNZIP: &str = include_str!("fixtures/gunzip.nf");

#[derive(Default)]
struct Output(String);

impl Host for Output {
    fn print(&mut self, text: &str) {
        self.0.push_str(text);
    }

    fn launch_dir(&self) -> String {
        "/launch".into()
    }
}

/// Runs a script, finishing every task in request order: each output pattern becomes a file
/// in `/work/<task>`, and each `eval` command answers `1.13`. The result has the requests and
/// what the workflow printed.
fn run(source: &str, options: &Options) -> Result<(Vec<TaskRequest>, String), String> {
    let parsed = parse_script(source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let module = Rc::new(lower_module(&parsed.tree, source).expect("lowers"));
    *module.directory.borrow_mut() = Some("/modules/test".into());
    let mut output = Output::default();
    let (mut run, mut queue) =
        Run::start(&module, options, &mut output).map_err(|error| error.to_string())?;
    let mut done = Vec::new();
    while !queue.is_empty() {
        let request = queue.remove(0);
        let outcome = TaskOutcome {
            id: request.id,
            files: request
                .output_patterns
                .iter()
                .map(|name| {
                    (
                        name.clone(),
                        PathValue::parse(&format!("/work/{}/{name}", request.id.0), "/"),
                    )
                })
                .collect(),
            eval: request.eval.iter().map(|_| "1.13".to_owned()).collect(),
            ..TaskOutcome::default()
        };
        done.push(request);
        let more = run
            .complete(outcome, &mut output)
            .map_err(|error| error.to_string())?;
        queue.extend(more);
    }
    run.finish(&mut output).map_err(|error| error.to_string())?;
    Ok((done, output.0))
}

const DOUBLE: &str = "process DOUBLE {\n    input:\n    val x\n    output:\n    val y\n    script:\n    y = x * 2\n    \"echo ${x}\"\n}\n";

#[test]
fn a_task_per_value() {
    let source = format!("{DOUBLE}workflow {{\n    channel.of(1, 2, 3) | DOUBLE | view\n}}");
    let (tasks, printed) = run(&source, &Options::default()).expect("runs");
    let names: Vec<&str> = tasks.iter().map(|task| task.name.as_str()).collect();
    assert_eq!(names, ["DOUBLE (1)", "DOUBLE (2)", "DOUBLE (3)"]);
    assert_eq!(tasks[0].script, "#!/bin/bash -ue\necho 1\n");
    assert_eq!(printed, "2\n4\n6\n");
}

#[test]
fn singletons_run_once() {
    let source = format!(
        "{DOUBLE}workflow {{\n    DOUBLE(5).view()\n    DOUBLE(channel.value(7)).view()\n}}"
    );
    let (tasks, printed) = run(&source, &Options::default()).expect("runs");
    let names: Vec<&str> = tasks.iter().map(|task| task.name.as_str()).collect();
    assert_eq!(names, ["DOUBLE", "DOUBLE"]);
    assert_eq!(printed, "10\n14\n");
}

#[test]
fn gunzip_stub_run() {
    let source = format!(
        "{GUNZIP}\nworkflow {{\n    GUNZIP(channel.of([[id: 'S1'], file('/data/sample.fastq.gz')]))\n    \
         GUNZIP.out.gunzip.view()\n    GUNZIP.out.versions_gunzip.view()\n}}"
    );
    let options = Options {
        stub: true,
        ..Options::default()
    };
    let (tasks, printed) = run(&source, &options).expect("runs");
    let [task] = tasks.as_slice() else {
        panic!("one task")
    };
    assert_eq!(task.name, "GUNZIP (sample.fastq.gz)");
    assert_eq!(task.script, "#!/bin/bash -ue\ntouch sample.fastq\n");
    assert_eq!(task.output_patterns, ["sample.fastq"]);
    assert_eq!(
        task.staged,
        [(
            PathValue::parse("/data/sample.fastq.gz", "/"),
            "sample.fastq.gz".to_owned()
        )]
    );
    assert_eq!(
        printed,
        "[[id:S1], /work/1/sample.fastq]\n[GUNZIP, gunzip, 1.13]\n"
    );
}

/// A config from its text.
fn config(text: &str) -> nextflow_config::Config {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let file = directory.path().join("nextflow.config");
    std::fs::write(&file, text).expect("writes the config");
    let options = nextflow_config::Options {
        profiles: None,
        params: nextflow_evaluator::value::Value::map([]),
        project_dir: "/project".into(),
        launch_dir: "/launch".into(),
    };
    nextflow_config::load(&[file], &options, &mut Output::default()).expect("loads")
}

#[test]
fn config_settings_reach_the_task() {
    let source = format!(
        "{GUNZIP}\nworkflow {{\n    GUNZIP(channel.of([[id: 'S1'], file('/data/sample.fastq.gz')]))\n}}"
    );
    let text = "process {\n    cpus = 2\n    ext.args = '-v'\n    resourceLimits = [cpus: 4]\n\
                withName: 'GUNZIP' {\n        cpus = { 8 * task.attempt }\n        \
                ext.prefix = { \"${meta.id}.custom\" }\n    }\n}\n";
    let options = Options {
        stub: true,
        config: config(text),
        ..Options::default()
    };
    let (tasks, _) = run(&source, &options).expect("runs");
    let [task] = tasks.as_slice() else {
        panic!("one task")
    };
    assert_eq!(task.script, "#!/bin/bash -ue\ntouch S1.custom.fastq\n");
    let cpus = task
        .directives
        .iter()
        .find(|directive| &*directive.name == "cpus")
        .map(|directive| directive.arguments[0].to_string());
    assert_eq!(cpus.as_deref(), Some("4"));
}

#[test]
fn workflow_metadata_and_completion_handlers() {
    let source = format!(
        "{DOUBLE}workflow {{\n    DOUBLE(channel.of(1, 2))\n    \
         println \"${{workflow.toMap().runName}} ${{workflow.profile}} ${{workflow.manifest.name}} ${{launchDir}} ${{projectDir}}\"\n    \
         workflow.onComplete {{ log.info \"done ${{workflow.success}} ${{workflow.stats.succeededCount}}\" }}\n}}"
    );
    let options = Options {
        config: config("manifest.name = 'demo'\n"),
        ..Options::default()
    };
    let (_, printed) = run(&source, &options).expect("runs");
    assert_eq!(printed, "iunctus standard demo /launch /\ndone true 2\n");
}

#[test]
fn topic_channels_collect_process_outputs() {
    let process = "process P {\n    input:\n    val x\n    output:\n    val y, topic: 'vers'\n    script:\n    y = \"A${x}\"\n    'true'\n}\n";
    let source = format!(
        "{process}workflow {{\n    P(channel.of(1, 2))\n    channel.topic('vers').toList().view()\n    \
         channel.topic('nothing').toList().view()\n}}"
    );
    let (_, printed) = run(&source, &Options::default()).expect("runs");
    let mut lines: Vec<&str> = printed.lines().collect();
    lines.sort_unstable();
    assert_eq!(lines, ["[A1, A2]", "[]"]);
}

#[test]
fn text_paths_are_input_files() {
    let process =
        "process P {\n    input:\n    path x\n    output:\n    val x\n    script:\n    'true'\n}\n";
    let source = format!("{process}workflow {{\n    P('https://host/data/a.txt').view()\n}}");
    let (tasks, printed) = run(&source, &Options::default()).expect("runs");
    assert_eq!(tasks[0].staged[0].1, "a.txt");
    assert_eq!(printed, "a.txt\n");
    let source = format!("{process}workflow {{\n    P('a.txt')\n}}");
    let error = run(&source, &Options::default()).unwrap_err();
    assert!(
        error.contains("Not a valid path value type: java.lang.String (a.txt)"),
        "{error}"
    );
}

#[test]
fn staged_names_expand_wildcards() {
    let staged = |pattern: &str, files: &str| {
        let process = format!(
            "process P {{\n    input:\n    path x, stageAs: '{pattern}'\n    script:\n    'true'\n}}\n"
        );
        let source = format!("{process}workflow {{\n    P({files})\n}}");
        let (tasks, _) = run(&source, &Options::default()).expect("runs");
        let names: Vec<String> = tasks[0]
            .staged
            .iter()
            .map(|(_, name)| name.clone())
            .collect();
        names.join(" ")
    };
    let two = "[file('/d/a.txt'), file('/d/b.txt')]";
    assert_eq!(staged("x*.txt", two), "x1.txt x2.txt");
    assert_eq!(staged("x*.txt", "file('/d/a.txt')"), "x.txt");
    assert_eq!(staged("in??.fa", two), "in01.fa in02.fa");
    assert_eq!(staged("dir*/*", two), "dir1/a.txt dir2/b.txt");
    assert_eq!(staged("data", two), "data1 data2");
    assert_eq!(staged("*", two), "a.txt b.txt");
}

#[test]
fn missing_outputs_and_failures() {
    let process = "process P {\n    input:\n    val x\n    output:\n    path 'out.txt'\n    script:\n    'x'\n}\n";
    let parsed_source = format!("{process}workflow {{\n    P(1)\n}}");
    let parsed = parse_script(&parsed_source);
    let module = Rc::new(lower_module(&parsed.tree, &parsed_source).expect("lowers"));
    let mut output = Output::default();
    let (mut run, requests) =
        Run::start(&module, &Options::default(), &mut output).expect("starts");
    let outcome = TaskOutcome {
        id: requests[0].id,
        ..TaskOutcome::default()
    };
    let error = run.complete(outcome, &mut output).unwrap_err().to_string();
    assert_eq!(
        error,
        "MissingFileException: Missing output file(s) `out.txt` expected by process `P`"
    );
    let (mut run, requests) =
        Run::start(&module, &Options::default(), &mut output).expect("starts");
    let outcome = TaskOutcome {
        id: requests[0].id,
        exit_status: 1,
        ..TaskOutcome::default()
    };
    let error = run.complete(outcome, &mut output).unwrap_err().to_string();
    assert_eq!(
        error,
        "ProcessFailedException: Process `P` terminated with an error exit status (1)"
    );
}
