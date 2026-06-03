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
    *module.path.borrow_mut() = Some("/modules/test/main.nf".into());
    *module.source.borrow_mut() = Some(source.into());
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

/// A tag of the config that fails names the task by its index, as Nextflow's
/// `TaskRun.getName` ignores the error.
#[test]
fn tags_that_fail_are_left_out() {
    let source = format!("{DOUBLE}workflow {{\n    DOUBLE(channel.of(1, 2))\n}}");
    let text = "process {\n    withName: 'DOUBLE' {\n        tag = { \"${meta.id}\" }\n    }\n}\n";
    let options = Options {
        config: config(text),
        ..Options::default()
    };
    let (tasks, _) = run(&source, &options).expect("runs");
    let names: Vec<&str> = tasks.iter().map(|task| task.name.as_str()).collect();
    assert_eq!(names, ["DOUBLE (1)", "DOUBLE (2)"]);
}

/// `${meta.id}` outside a string in a config closure is a call with one closure, which
/// Nextflow's legacy config parser takes for a config block: the closure gives null, as
/// Nextflow 26.04.6 prints for nf-core/metaboigniter's `ext.prefix = { ${meta.id} }`.
#[test]
fn config_blocks_in_config_closures_give_null() {
    let source = "process P {\n    input:\n    val meta\n    output:\n    val x\n    script:\n    \
                  x = \"${task.ext.prefix}\"\n    'true'\n}\n\
                  workflow {\n    P([id: 'a']).view()\n}";
    let text = "process {\n    withName: 'P' {\n        ext.prefix = { ${meta.id} }\n    }\n}\n";
    let options = Options {
        config: config(text),
        ..Options::default()
    };
    let (_, printed) = run(source, &options).expect("runs");
    assert_eq!(printed, "null\n");
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

/// A staged name may use a `val` input declared after it, as atacseq's IGV process does.
#[test]
fn staged_names_read_later_values() {
    let source = "process P {\n    input:\n    path(\"${dir}/*\")\n    val dir\n    script:\n    \
                  'true'\n}\nworkflow {\n    P([file('/d/a.txt'), file('/d/b.txt')], 'bigwig')\n}";
    let (tasks, _) = run(source, &Options::default()).expect("runs");
    let names: Vec<&str> = tasks[0]
        .staged
        .iter()
        .map(|(_, name)| name.as_str())
        .collect();
    assert_eq!(names, ["bigwig/a.txt", "bigwig/b.txt"]);
}

#[test]
fn typed_params_take_their_types_and_defaults() {
    let source = "params {\n    input: String? = null\n    n: Integer = 3\n    flag: Boolean\n    outdir: String\n}\n\
                  workflow {\n    println \"${params.input} ${params.n + 1} ${params.flag} ${params.outdir}\"\n}";
    let with = |text: &str| Options {
        config: config(text),
        ..Options::default()
    };
    let (_, printed) = run(source, &with("params.outdir = 'x'\nparams.n = '7'\n")).expect("runs");
    assert_eq!(printed, "null 8 false x\n");
    let error = run(source, &with("")).unwrap_err();
    assert!(error.ends_with("IllegalArgumentException: Parameter `outdir` is required but was not specified on the command line, params file, or config"), "{error}");
}

#[test]
fn one_file_is_a_path_unless_the_arity_allows_more() {
    let class = |input: &str, files: &str| {
        let process = format!(
            "process P {{\n    input:\n    {input}\n    output:\n    val y\n    script:\n    y = x instanceof Path\n    'true'\n}}\n"
        );
        let source = format!("{process}workflow {{\n    P({files}).view()\n}}");
        run(&source, &Options::default()).expect("runs").1
    };
    assert_eq!(class("path x", "[file('/d/a.txt')]"), "true\n");
    assert_eq!(
        class("path x, arity: '1..*'", "file('/d/a.txt')"),
        "false\n"
    );
}

#[test]
fn tuples_of_another_size_are_errors() {
    let process = "process P {\n    input:\n    tuple val(a), val(b)\n    script:\n    'true'\n}\n";
    let source = format!(
        "{process}workflow W {{\n    P(channel.of([1, 2, 3]))\n}}\nworkflow {{\n    W()\n}}"
    );
    let error = run(&source, &Options::default()).unwrap_err();
    // The message of Nextflow 26.04.6, which names the process by its fully qualified name.
    assert!(
        error.ends_with(
            "ProcessUnrecoverableException: Input tuple does not match tuple declaration in \
             process `W:P` -- offending value: [1, 2, 3]"
        ),
        "{error}"
    );
}

#[test]
fn a_staged_file_iterates_as_itself() {
    let process = "process P {\n    input:\n    path reads\n    output:\n    val y\n    script:\n    y = \"${reads[0]} ${reads.join(',')} ${reads.collectMany { [it] }}\"\n    'true'\n}\n";
    let source = format!(
        "{process}workflow {{\n    P([file('/d/a.txt')]).view()\n    println file('/x/y/a.txt')[1]\n}}"
    );
    let (_, printed) = run(&source, &Options::default()).expect("runs");
    assert_eq!(printed, "y\na.txt a.txt [a.txt]\n");
}

#[test]
fn closure_arguments_give_their_result() {
    let source = format!("{DOUBLE}workflow {{\n    DOUBLE {{ channel.of(4) }}.view()\n}}");
    let (_, printed) = run(&source, &Options::default()).expect("runs");
    assert_eq!(printed, "8\n");
}

#[test]
fn manifest_settings_are_text_and_null_has_methods() {
    let source = "workflow {\n    println([null.equals(null), null.toString(), workflow.manifest.doi, \
                  workflow.manifest.doi.tokenize(','), workflow.manifest.mainScript, workflow.manifest.version])\n}";
    let options = Options {
        config: config("manifest {\n    name = 'demo'\n    doi = ['10.1/a', '10.2/b']\n}\n"),
        ..Options::default()
    };
    let (_, printed) = run(source, &options).expect("runs");
    assert_eq!(
        printed,
        "[true, null, [10.1/a, 10.2/b], [[10.1/a,  10.2/b]], main.nf, null]\n"
    );
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

#[test]
fn a_return_in_a_workflow_leaves_its_assigned_emits_missing() {
    let workflow = "workflow W {\n    take:\n    x\n\n    main:\n    if (true) {\n        return\n    \
                    }\n    y = x.map { v -> v * 2 }\n\n    emit:\n    EMITS\n}\n";
    // Nextflow 26.04.6 fails so; an emit that names a variable is still found.
    let source = format!(
        "{}workflow {{\n    W(channel.of(1))\n    W.out.out.view()\n}}",
        workflow.replace("EMITS", "out = y\n    x")
    );
    let error = run(&source, &Options::default()).unwrap_err();
    assert!(
        error.ends_with("MissingValueException: Missing workflow output parameter: out"),
        "{error}"
    );
    let source = format!(
        "{}workflow {{\n    W(channel.of(1))\n    W.out.x.view()\n}}",
        workflow.replace("EMITS", "x")
    );
    let (_, printed) = run(&source, &Options::default()).expect("runs");
    assert_eq!(printed, "1\n");
    // An emit with a value of its own and the name of a take gives the take, as Nextflow
    // 26.04.6 prints `1` with both parsers.
    let source = format!(
        "{}workflow {{\n    W(channel.of(1))\n    W.out.x.view()\n}}",
        workflow.replace("EMITS", "x = y")
    );
    let (_, printed) = run(&source, &Options::default()).expect("runs");
    assert_eq!(printed, "1\n");
}

/// A workflow that emits the outputs of a call emits their one channel, as Nextflow's
/// `collectOutputs` does, so that operators take it as a channel.
#[test]
fn emits_of_call_outputs_are_their_channel() {
    let source = "process P {\n    output:\n    val 'hi'\n    script:\n    'true'\n}\n\
                  workflow W {\n    main:\n    P()\n    emit:\n    P.out\n}\n\
                  workflow {\n    W()\n    W.out.concat(channel.of(1)).view()\n}";
    let (_, printed) = run(source, &Options::default()).expect("runs");
    assert_eq!(printed, "hi\n1\n");
}

/// An output that reads a variable that only the script sets fails in a stub run, as in
/// Nextflow 26.04.6, which says "No such variable: outformat", or for a `val` output "Missing
/// value declared as output parameter: outformat".
#[test]
fn stub_runs_leave_the_variables_of_scripts_unset() {
    let source = "process P {\n    input:\n    val x\n    output:\n    path(\"*.${outformat}\")\n    \
                  script:\n    outformat = 'fastq'\n    \"touch a.${outformat}\"\n    stub:\n    \
                  \"touch a.fastq\"\n}\nworkflow {\n    P(channel.of(1))\n}";
    let stub = Options {
        stub: true,
        ..Options::default()
    };
    let error = run(source, &stub).unwrap_err();
    assert_eq!(
        error,
        "/modules/test/main.nf:5:15: MissingPropertyException: No such variable: outformat"
    );
    run(source, &Options::default()).expect("runs");
    // A `val` output of such a variable is a missing value.
    let source = source.replace("path(\"*.${outformat}\")", "val(outformat)");
    let error = run(&source, &stub).unwrap_err();
    assert!(
        error.ends_with(
            "MissingValueException: Missing value declared as output parameter: outformat"
        ),
        "{error}"
    );
}

/// `each` inputs start a task for each combination of their values with the other inputs, as
/// Nextflow's `ForwardClosure` does; a queue channel gives all its values.
#[test]
fn each_inputs_start_a_task_for_each_value() {
    let process = |name: &str| {
        format!(
            "process {name} {{\n    input:\n    val x\n    each y\n    script:\n    \
             \"echo ${{x}}${{y}}\"\n}}\n"
        )
    };
    let source = format!(
        "{}{}workflow {{\n    P(channel.of('a', 'b'), channel.of(1, 2))\n    \
         Q(channel.value('c'), [3, 4])\n}}",
        process("P"),
        process("Q")
    );
    let (tasks, _) = run(&source, &Options::default()).expect("runs");
    let mut scripts: Vec<&str> = tasks
        .iter()
        .map(|task| task.script.lines().last().unwrap_or_default())
        .collect();
    scripts.sort_unstable();
    assert_eq!(
        scripts,
        [
            "echo a1", "echo a2", "echo b1", "echo b2", "echo c3", "echo c4"
        ]
    );
}

/// A `path` output with a closure takes its value when the task runs, and a list gives a
/// pattern for each element, as nf-core/metaboigniter names its outputs after `meta.id`.
#[test]
fn path_outputs_take_closures_and_lists() {
    let source = "process P {\n    input:\n    val ids\n    output:\n    \
                  path({ ids.collect { 'out/' + it + '.xml' } })\n    script:\n    'true'\n}\n\
                  workflow {\n    P(['a', 'b']).view { files -> files*.name }\n}";
    let (tasks, printed) = run(source, &Options::default()).expect("runs");
    assert_eq!(tasks[0].output_patterns, ["out/a.xml", "out/b.xml"]);
    assert_eq!(printed, "[a.xml, b.xml]\n");
}

/// A `set` at the top of a script assigns a variable of the script, which the workflow reads,
/// as in Nextflow 26.04.6 with the legacy parser.
#[test]
fn a_set_at_the_top_assigns_a_script_variable() {
    let source = "Channel.of(2).map { it * 2 }.set { ch_b }\nworkflow {\n    ch_b.view()\n}";
    let (_, printed) = run(source, &Options::default()).expect("runs");
    assert_eq!(printed, "4\n");
}

/// An error of a completion handler is logged and the run still succeeds, as in Nextflow
/// 26.04.6, which prints "Failed to invoke `workflow.onComplete` event handler".
#[test]
fn completion_handlers_that_fail_are_logged() {
    let source = "workflow {\n    workflow.onComplete { println('first'); NfcoreTemplate.summary(workflow) }\n    \
                  workflow.onComplete { println('second') }\n    channel.of(1).view()\n}";
    let (_, printed) = run(source, &Options::default()).expect("runs");
    let lines: Vec<&str> = printed.lines().collect();
    assert_eq!(lines[0], "1");
    assert_eq!(lines[1], "first");
    assert_eq!(
        lines[2],
        "ERROR ~ Failed to invoke `workflow.onComplete` event handler: /modules/test/main.nf:2:45: \
         MissingPropertyException: No such variable: NfcoreTemplate"
    );
    assert_eq!(lines[3], "second");
}

/// Completion handlers read the value of a value channel with `getVal()`, as in Nextflow
/// 26.04.6.
#[test]
fn completion_handlers_read_value_channels() {
    let source = "workflow {\n    def reports = channel.of(1, 2).toList()\n    \
                  workflow.onComplete { println(\"reports ${reports.getVal()} ${reports.val.size()}\") }\n}";
    let (_, printed) = run(source, &Options::default()).expect("runs");
    assert_eq!(printed, "reports [1, 2] 2\n");
}
