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

    /// Prints what would be written, as `/launch` does not exist.
    fn write(
        &mut self,
        path: &PathValue,
        bytes: &[u8],
    ) -> Result<(), nextflow_evaluator::interpreter::Error> {
        let text = String::from_utf8_lossy(bytes);
        self.0.push_str(&format!("write {}: {text}\n", path.uri()));
        Ok(())
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
fn limits_process_tasks() {
    for configured in [false, true] {
        let source = "process P {\n maxForks 1\n input:\n val x\n output:\n val x\n script:\n 'true'\n}\nprocess Q {\n input:\n val x\n script:\n 'true'\n}\nworkflow {\n P(channel.of(1, 2, 3, 4)).view()\n Q(channel.of(5, 6, 7))\n}";
        let parsed = parse_script(source);
        let module = Rc::new(lower_module(&parsed.tree, source).unwrap());
        let options = Options {
            config: if configured {
                config("process { withName: P { maxForks = 2 } }")
            } else {
                nextflow_config::Config::default()
            },
            ..Options::default()
        };
        let limit = if configured { 2 } else { 1 };
        let mut output = Output::default();
        let (mut run, mut queue) = Run::start(&module, &options, &mut output).unwrap();
        assert_eq!(
            queue.iter().filter(|task| &*task.process == "P").count(),
            limit
        );
        assert_eq!(queue.iter().filter(|task| &*task.process == "Q").count(), 3);
        let mut completed = 0;
        while !queue.is_empty() {
            let request = queue.remove(0);
            queue.extend(
                run.complete(
                    TaskOutcome {
                        id: request.id,
                        ..TaskOutcome::default()
                    },
                    &mut output,
                )
                .unwrap(),
            );
            assert!(queue.iter().filter(|task| &*task.process == "P").count() <= limit);
            completed += 1;
        }
        run.finish(&mut output).unwrap();
        assert_eq!(completed, 7);
        assert_eq!(output.0, "1\n2\n3\n4\n");
    }
}

#[test]
fn limits_each_retries() {
    let source = "process P {\n maxForks 1\n errorStrategy 'retry'\n maxRetries 1\n input:\n each x\n output:\n val x\n script:\n 'true'\n}\nworkflow { P([1, 2, 3]).view() }";
    let parsed = parse_script(source);
    let module = Rc::new(lower_module(&parsed.tree, source).unwrap());
    let mut output = Output::default();
    let (mut run, mut queue) = Run::start(&module, &Options::default(), &mut output).unwrap();
    assert_eq!(queue.len(), 1);
    let first = queue.remove(0);
    let retry = run
        .complete(
            TaskOutcome {
                id: first.id,
                exit_status: 1,
                ..TaskOutcome::default()
            },
            &mut output,
        )
        .unwrap();
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].id, first.id);
    assert_eq!(retry[0].attempt, 2);
    queue.extend(retry);
    let mut completed = 0;
    while let Some(request) = queue.pop() {
        queue.extend(
            run.complete(
                TaskOutcome {
                    id: request.id,
                    ..TaskOutcome::default()
                },
                &mut output,
            )
            .unwrap(),
        );
        assert!(queue.len() <= 1);
        completed += 1;
    }
    run.finish(&mut output).unwrap();
    assert_eq!(completed, 3);
    assert!(output.0.ends_with("1\n2\n3\n"));
}

#[test]
fn rejects_active_directives() {
    for (name, value) in [
        ("fair", "true"),
        ("maxErrors", "2"),
        ("containerOptions", "'--privileged'"),
        ("clusterOptions", "'--export=TOKEN=value'"),
        ("clusterOptions", "'-q batch -v TOKEN=value'"),
        ("clusterOptions", "'--partition --export=TOKEN=value'"),
        ("module", "'samtools'"),
        ("arch", "[name: 'linux/x86_64', target: 'cascadelake']"),
        ("pod", "[env: 'TOKEN', value: 'value']"),
        (
            "pod",
            "[[nodeSelector: 'disk=ssd'], [secret: 'token/key', env: 'TOKEN']]",
        ),
        ("pod", "[volumeClaim: 'data', mountPath: '/data']"),
        ("stageOutMode", "'move'"),
    ] {
        let source =
            format!("process P {{\n {name}({value})\n script:\n 'true'\n}}\nworkflow {{ P() }}");
        let error = run(&source, &Options::default()).expect_err("unsupported directive");
        assert!(
            error.contains(&format!("process directive `{name}`")),
            "{error}"
        );
        let source = "process P {\n script:\n 'true'\n}\nworkflow { P() }";
        let options = Options {
            config: config(&format!("process.{name} = {value}")),
            ..Options::default()
        };
        let error = run(source, &options).expect_err("unsupported config directive");
        assert!(
            error.contains(&format!("process directive `{name}`")),
            "{error}"
        );
    }
}

#[test]
fn accepts_scheduling_hints() {
    for (name, value) in [
        ("executor", "'slurm'"),
        ("queue", "'batch'"),
        (
            "clusterOptions",
            "'--exclusive --partition=batch --account project'",
        ),
        ("clusterOptions", "['-q batch', '--qos normal']"),
        ("penv", "'smp'"),
        ("machineType", "'n1-standard-4'"),
        ("arch", "'arm64'"),
        ("arch", "[name: 'linux/x86_64']"),
        ("resourceLabels", "[region: 'local']"),
        ("scratch", "true"),
        ("scratch", "'/scratch'"),
        ("pod", "[nodeSelector: 'disk=ssd']"),
        (
            "pod",
            "[[affinity: [nodeAffinity: [:]]], [toleration: [key: 'workload']]]",
        ),
        ("array", "10"),
    ] {
        for configured in [false, true] {
            let directive = if configured {
                String::new()
            } else {
                format!(" {name}({value})\n")
            };
            let source = format!(
                "process P {{\n{directive} input:\n val x\n output:\n val x\n script:\n 'true'\n}}\nworkflow {{ P(channel.of(1, 2)).view() }}"
            );
            let options = Options {
                config: if configured {
                    config(&format!("process.{name} = {value}"))
                } else {
                    nextflow_config::Config::default()
                },
                ..Options::default()
            };
            let (tasks, output) = run(&source, &options).expect(name);
            assert_eq!(tasks.len(), 2, "{name}");
            let diagnostic = format!("INFO: process directive `{name}` is ignored");
            assert_eq!(output.matches(&diagnostic).count(), 1, "{output}");
            assert!(output.ends_with("1\n2\n"), "{output}");
        }
    }
}

#[test]
fn accepts_inactive_directives() {
    let source = "process P {\n beforeScript ''\n afterScript ''\n fair false\n maxErrors(-1)\n storeDir ''\n maxForks null\n containerOptions ''\n clusterOptions ''\n queue ''\n module []\n arch null\n stageOutMode 'copy'\n secret []\n executor null\n script:\n 'true'\n}\nworkflow { P() }";
    let (tasks, _) = run(source, &Options::default()).expect("inactive directives");
    assert_eq!(tasks.len(), 1);
}

#[test]
fn accepts_inactive_config() {
    let source = "process P {\n script:\n 'true'\n}\nworkflow { P() }";
    let options = Options {
        config: config(
            "process {\n storeDir = ''\n maxForks = null\n containerOptions = ''\n clusterOptions = ''\n queue = ''\n module = []\n arch = null\n stageOutMode = 'copy'\n secret = []\n executor = null\n}\n",
        ),
        ..Options::default()
    };
    let (tasks, _) = run(source, &options).expect("inactive config directives");
    assert_eq!(tasks.len(), 1);
}

#[test]
fn rejects_enabled_environment() {
    let name = "spack";
    let source = format!(
        "process P {{\n {name} 'environment.yml'\n script:\n 'true'\n}}\nworkflow {{ P() }}"
    );
    let (tasks, _) = run(&source, &Options::default()).expect("disabled environment");
    assert_eq!(tasks.len(), 1);
    let options = Options {
        config: config(&format!("{name}.enabled = true")),
        ..Options::default()
    };
    let error = run(&source, &options).expect_err("unsupported environment");
    assert!(
        error.contains(&format!("process directive `{name}`")),
        "{error}"
    );
}

#[test]
fn accepts_unlimited_errors() {
    // nf-core `conf/base.config` sets `maxErrors = '-1'` as a string.
    let source = "process P {\n script:\n 'true'\n}\nworkflow { P() }";
    let options = Options {
        config: config("process.maxErrors = '-1'"),
        ..Options::default()
    };
    let (tasks, _) = run(source, &options).expect("unlimited maxErrors");
    assert_eq!(tasks.len(), 1);
}

#[test]
fn stops_closed_process() {
    for inputs in [
        "channel.empty(), channel.value()",
        "channel.value(), channel.empty()",
    ] {
        let source = format!(
            "process P {{\n input:\n val x\n val y\n output:\n val x\n script:\n 'true'\n}}\nworkflow {{\n P({inputs}).ifEmpty('empty').view()\n}}\n"
        );
        let (tasks, printed) = run(&source, &Options::default()).expect("closes without waiting");
        assert!(tasks.is_empty());
        assert_eq!(printed, "empty\n");
    }
}

#[test]
fn creates_value_tasks() {
    let source = format!("{DOUBLE}workflow {{\n    channel.of(1, 2, 3) | DOUBLE | view\n}}");
    let (tasks, printed) = run(&source, &Options::default()).expect("runs");
    let names: Vec<&str> = tasks.iter().map(|task| task.name.as_str()).collect();
    assert_eq!(names, ["DOUBLE (1)", "DOUBLE (2)", "DOUBLE (3)"]);
    assert_eq!(tasks[0].script, "#!/bin/bash -ue\necho 1\n");
    assert_eq!(printed, "2\n4\n6\n");
}

#[test]
fn preserves_round_inputs() {
    let source = "process P {\n    input:\n    tuple env('SAMPLE'), stdin\n    script:\n    'cat'\n}\n\
                  workflow {\n    P(channel.of(['one', 'first\\n'], ['two', 'second\\n']))\n}\n";
    let (tasks, _) = run(source, &Options::default()).expect("runs");
    assert_eq!(tasks.len(), 2);
    for (task, (sample, stdin)) in tasks.iter().zip([("one", "first\n"), ("two", "second\n")]) {
        assert_eq!(task.input_environment.len(), 1);
        assert_eq!(&*task.input_environment[0].0, "SAMPLE");
        assert_eq!(task.input_environment[0].1.to_string(), sample);
        assert_eq!(
            task.stdin.as_ref().map(ToString::to_string).as_deref(),
            Some(stdin)
        );
        assert!(task.staged.is_empty());
    }
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
fn applies_task_config() {
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
fn excludes_failed_tags() {
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
fn evaluates_config_blocks() {
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
fn exposes_workflow_metadata() {
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

/// `workflow.container` is what the `process` scope sets, as Nextflow 26.04.6 prints it.
#[test]
fn shows_config_containers() {
    let source = "workflow {\n    println workflow.container\n}";
    let shown = |text: &str| {
        let options = Options {
            config: config(text),
            ..Options::default()
        };
        run(source, &options).expect("runs").1
    };
    assert_eq!(shown(""), "[:]\n");
    assert_eq!(shown("process.container = 'ubuntu'\n"), "ubuntu\n");
    let selectors = "process {\n    container = 'ubuntu'\n    withName: 'FOO' { container = 'x' }\n    \
                     withLabel: 'big' { container = 'y' }\n    withName: 'BAR' { cpus = 2 }\n}\n";
    assert_eq!(
        shown(selectors),
        "[FOO:x, withLabel:big:y, default:ubuntu]\n"
    );
}

#[test]
fn collects_topic_outputs() {
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
fn resolves_text_inputs() {
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

/// The legacy `file` qualifier writes each value that is not a path into a file `input.N`, as
/// with Nextflow 26.04.6 and the legacy parser.
#[test]
fn writes_value_inputs() {
    let process = "process P {\n    input:\n    path a\n    file x\n    output:\n    val x\n    \
                   script:\n    'true'\n}\n";
    let source = format!("{process}workflow {{\n    P('https://host/a.txt', 'Tcell').view()\n}}");
    let (tasks, printed) = run(&source, &Options::default()).expect("runs");
    assert_eq!(tasks[0].staged[1].1, "input.2");
    assert_eq!(
        printed,
        "write /launch/work/tmp/161e9be40764b3be5916702860e57f75/input.2: Tcell\ninput.2\n"
    );
}

#[test]
fn expands_staged_wildcards() {
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

/// A staged input iterates the parts of its staged name, as the stub of CAT_CAT reads them:
/// Nextflow 26.04.6 gives `[[1, a.txt], [b.txt]]`.
#[test]
fn iterates_staged_inputs() {
    let process = "process P {\n    input:\n    path x, stageAs: '?/*'\n    path y\n    script:\n    \
                   println([x.collect { it.toString() }, y.collect { it.toString() }])\n    \
                   'true'\n}\n";
    let source = format!("{process}workflow {{\n    P(file('/d/a.txt'), file('/d/b.txt'))\n}}");
    let (_, printed) = run(&source, &Options::default()).expect("runs");
    assert_eq!(printed, "[[1, a.txt], [b.txt]]\n");
}

/// A staged name may use a `val` input declared after it, as atacseq's IGV process does.
#[test]
fn reads_later_values() {
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
fn applies_typed_defaults() {
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
fn shapes_path_arity() {
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
fn enforces_output_arity() {
    let collect = |arity: &str, count: usize| -> Result<String, String> {
        let source = format!(
            "process P {{\n output:\n path '*.txt', arity: '{arity}'\n script:\n 'true'\n}}\nworkflow {{\n P().map {{ it instanceof Path ? 'path' : \"list:${{it.size()}}\" }}.view()\n}}\n"
        );
        let parsed = parse_script(&source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let module = Rc::new(lower_module(&parsed.tree, &source).expect("lowers"));
        let mut host = Output::default();
        let (mut run, requests) = Run::start(&module, &Options::default(), &mut host)
            .map_err(|error| error.to_string())?;
        let request = &requests[0];
        run.complete(
            TaskOutcome {
                id: request.id,
                files: (0..count)
                    .map(|index| {
                        let name = format!("{index}.txt");
                        (
                            name.clone(),
                            PathValue::parse(&format!("/work/{name}"), "/"),
                        )
                    })
                    .collect(),
                ..TaskOutcome::default()
            },
            &mut host,
        )
        .map_err(|error| error.to_string())?;
        run.finish(&mut host).map_err(|error| error.to_string())?;
        Ok(host.0)
    };
    for (arity, count, expected) in [
        ("1", 1, "path\n"),
        ("2", 2, "list:2\n"),
        ("0..2", 0, "list:0\n"),
        ("0..2", 1, "list:1\n"),
        ("0..*", 0, "list:0\n"),
        ("1..*", 1, "list:1\n"),
        ("1..*", 3, "list:3\n"),
    ] {
        assert_eq!(collect(arity, count).expect("within bounds"), expected);
    }
    for (arity, count) in [
        ("1", 0),
        ("1", 2),
        ("2", 1),
        ("2", 3),
        ("0..2", 3),
        ("1..*", 0),
    ] {
        let error = collect(arity, count).unwrap_err();
        assert!(
            error.contains("Output arity") || error.contains("Missing output file"),
            "{arity}, {count}: {error}"
        );
    }
    for arity in ["bad", "2..1", "-1", "1..bad", "0", "0..0", "0..1"] {
        let error = collect(arity, 1).unwrap_err();
        assert!(error.contains("Invalid output arity"), "{error}");
    }
}

#[test]
fn rejects_tuple_sizes() {
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
fn iterates_staged_file() {
    let process = "process P {\n    input:\n    path reads\n    output:\n    val y\n    script:\n    y = \"${reads[0]} ${reads.join(',')} ${reads.collectMany { [it] }}\"\n    'true'\n}\n";
    let source = format!(
        "{process}workflow {{\n    P([file('/d/a.txt')]).view()\n    println file('/x/y/a.txt')[1]\n}}"
    );
    let (_, printed) = run(&source, &Options::default()).expect("runs");
    assert_eq!(printed, "y\na.txt a.txt [a.txt]\n");
}

#[test]
fn evaluates_closure_arguments() {
    let source = format!("{DOUBLE}workflow {{\n    DOUBLE {{ channel.of(4) }}.view()\n}}");
    let (_, printed) = run(&source, &Options::default()).expect("runs");
    assert_eq!(printed, "8\n");
}

#[test]
fn coerces_manifest_settings() {
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
fn reports_output_failures() {
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
fn excludes_unreturned_emits() {
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
fn resolves_call_emits() {
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
fn excludes_stub_variables() {
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
fn expands_each_inputs() {
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

#[test]
fn bounds_process_requests() {
    for (inputs, expected) in [("val x\n", 2000), ("each x\n each y\n", 2500)] {
        let each = expected == 2500;
        let arguments = if each {
            "(1..50), (1..50)"
        } else {
            "channel.of(1..2000)"
        };
        let script = if each {
            "\"echo ${x}:${y}\""
        } else {
            "\"echo ${x}\""
        };
        let source = format!(
            "process P {{\n input:\n {inputs} script:\n {script}\n}}\nworkflow {{ P({arguments}) }}"
        );
        let parsed = parse_script(&source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let module = Rc::new(lower_module(&parsed.tree, &source).expect("lowers"));
        let mut host = Output::default();
        let (mut run, requests) =
            Run::start(&module, &Options::default(), &mut host).expect("starts");
        assert_eq!(requests.len(), 128);
        let mut queue: std::collections::VecDeque<_> = requests.into();
        let mut count = 0;
        while let Some(request) = queue.pop_front() {
            let command = if each {
                format!("echo {}:{}", count % 50 + 1, count / 50 + 1)
            } else {
                format!("echo {}", count + 1)
            };
            assert_eq!(request.script.lines().last(), Some(command.as_str()));
            count += 1;
            queue.extend(
                run.complete(
                    TaskOutcome {
                        id: request.id,
                        ..TaskOutcome::default()
                    },
                    &mut host,
                )
                .expect("completes"),
            );
            assert!(queue.len() <= 128, "unbounded outstanding requests");
        }
        assert_eq!(count, expected);
        run.finish(&mut host).expect("finishes");
    }
}

/// A `path` output with a closure takes its value when the task runs, and a list gives a
/// pattern for each element, as nf-core/metaboigniter names its outputs after `meta.id`.
#[test]
fn evaluates_path_outputs() {
    let source = "process P {\n    input:\n    val ids\n    output:\n    \
                  path({ ids.collect { 'out/' + it + '.xml' } })\n    script:\n    'true'\n}\n\
                  workflow {\n    P(['a', 'b']).view { files -> files*.name }\n}";
    let (tasks, printed) = run(source, &Options::default()).expect("runs");
    assert_eq!(tasks[0].output_patterns, ["out/a.xml", "out/b.xml"]);
    assert_eq!(printed, "[a.xml, b.xml]\n");
}

/// Legacy syntax: `path "*.pdf" optional true` makes the output optional, as in
/// nf-core/quantms, cutandrun and ssds. Nextflow 21.10 and 23.04, the releases they name,
/// read it so; Nextflow 26.04.6 fails on it (decision 14).
#[test]
fn applies_output_options() {
    let source = "process P {\n    output:\n    path '*.pdf' optional true\n    path 'a.txt'\n    \
                  script:\n    'true'\n}\nworkflow {\n    P()\n}\n";
    let (tasks, _) = run(source, &Options::default()).expect("runs");
    assert_eq!(tasks.len(), 1);
}

/// Checks immediate workflow emits versus value channels bound when execution starts.
/// Completion-handler println emits an empty line after done, matching Nextflow 26.04.6.
#[test]
fn texts_of_channels() {
    let source = "workflow A {\n    main:\n    x = true\n    y = null\n    emit:\n    x = x\n    y = y\n}\n\
                  workflow {\n    A()\n    println(A.out.x.toString())\n    println(A.out.y.toString())\n    \
                  println(channel.value(1).toString())\n    println(channel.of(1).toString())\n    \
                  def q = channel.of(2)\n    workflow.onComplete { q.println(); println('done') }\n    \
                  channel.of(1).combine(A.out.y).view()\n}\n";
    let (_, printed) = run(source, &Options::default()).expect("runs");
    assert_eq!(
        printed,
        "DataflowVariable(value=true)\nDataflowVariable(value=null)\nDataflowVariable(value=null)\n\
         DataflowBroadcast around DataflowStream[?]\n[1, null]\n\ndone\n"
    );
}

/// Legacy syntax: directives inside `if` statements apply under their conditions, like the
/// `container` of nf-core modules that depends on `workflow.containerEngine`.
#[test]
fn evaluates_conditional_directives() {
    let source = "process P {\n    if (1 > 2) {\n        container 'a'\n    } else {\n        container 'b'\n        \
                  if (true) { cpus 2 }\n    }\n    script:\n    'true'\n}\nworkflow {\n    P()\n}\n";
    let (tasks, _) = run(source, &Options::default()).expect("runs");
    let directives: Vec<String> = tasks[0]
        .directives
        .iter()
        .map(|directive| format!("{} {:?}", directive.name, directive.arguments))
        .collect();
    assert_eq!(
        directives,
        ["container [String(\"b\")]", "cpus [Number(Integer(2))]"]
    );
}

/// A `set` at the top of a script assigns a variable of the script, which the workflow reads,
/// as in Nextflow 26.04.6 with the legacy parser.
#[test]
fn assigns_script_channels() {
    let source = "Channel.of(2).map { it * 2 }.set { ch_b }\nworkflow {\n    ch_b.view()\n}";
    let (_, printed) = run(source, &Options::default()).expect("runs");
    assert_eq!(printed, "4\n");
}

/// A `set` in an `if` of the script also assigns a variable of the script, as in the
/// TronFlow alignment pipeline, which Nextflow 26.04.6 runs.
#[test]
fn assigns_block_channels() {
    let source = "if (true) {\n    Channel.of(3).set { ch_b }\n} else {\n    Channel.of(4).set { ch_b }\n}\n\
                  workflow {\n    ch_b.view()\n}";
    let (_, printed) = run(source, &Options::default()).expect("runs");
    assert_eq!(printed, "3\n");
}

/// An error of a completion handler is logged and the run still succeeds, as in Nextflow
/// 26.04.6, which prints "Failed to invoke `workflow.onComplete` event handler".
#[test]
fn logs_completion_failures() {
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
fn reads_completion_channels() {
    let source = "workflow {\n    def reports = channel.of(1, 2).toList()\n    \
                  workflow.onComplete { println(\"reports ${reports.getVal()} ${reports.val.size()}\") }\n}";
    let (_, printed) = run(source, &Options::default()).expect("runs");
    assert_eq!(printed, "reports [1, 2] 2\n");
}

/// Legacy syntax: a directive after `when:` belongs to the guard, where it gives null, so
/// the process runs no task, as with Nextflow 26.04.6 for nf-core/ddamsproteomics.
#[test]
fn nulls_later_guard() {
    let source = "process P {\n    when: true\n\n    publishDir 'out'\n\n    input:\n    val x\n\n    \
                  script:\n    'true'\n}\nworkflow {\n    P(channel.of(1))\n}\n";
    let (tasks, _) = run(source, &Options::default()).expect("runs");
    assert!(tasks.is_empty());
}

/// A `shell:` section interpolates `!{expression}` and keeps `$` for the shell. Nextflow
/// 26.04.6 with the legacy parser writes `echo 1 $HOME 2 !x \n` for this process.
#[test]
fn interpolates_shell_placeholders() {
    let source = "process P {\n    input:\n    val x\n    output:\n    stdout\n    shell:\n    \
                  '''\n    echo !{x} $HOME !{x +\n    1} !x \\\\n\n    '''\n}\n\
                  workflow {\n    P(channel.of(1))\n}\n";
    let (tasks, _) = run(source, &Options::default()).expect("runs");
    assert_eq!(tasks[0].script, "#!/bin/bash -ue\necho 1 $HOME 2 !x \\n\n");
}

/// Legacy syntax: a workflow may emit several channels without names, which its caller reads
/// by position, and have its sections in any order, as Nextflow 26.04.6 runs them.
#[test]
fn accepts_reordered_sections() {
    let source = "workflow A {\n    take:\n    x\n    emit:\n    x.map { it + 1 }\n    x.map { it + 2 }\n    \
                  main:\n    println('main')\n}\n\
                  workflow B { main: println('b') }\n\
                  workflow {\n    A(channel.of(1))\n    A.out[1].view()\n    B()\n}";
    let (_, printed) = run(source, &Options::default()).expect("runs");
    assert_eq!(printed, "main\nb\n3\n");
}

/// Legacy syntax: a directive among the outputs, like the `publishDir` of MetaGT, is a
/// directive, as Nextflow 26.04.6 runs it.
#[test]
fn directives_among_outputs() {
    let source = "process P {\n    output:\n    path 'a.txt', emit: a\n\n    publishDir 'out', mode: 'copy'\n\n    \
                  script:\n    'touch a.txt'\n}\nworkflow {\n    P().a.view { it.name }\n}\n";
    let (tasks, printed) = run(source, &Options::default()).expect("runs");
    assert!(
        tasks[0]
            .directives
            .iter()
            .any(|directive| &*directive.name == "publishDir")
    );
    assert_eq!(printed, "a.txt\n");
}

/// Legacy syntax: `void name(...)` declares a function without a result.
#[test]
fn void_functions() {
    let source = "void hello() { println('hi') }\nworkflow {\n    hello()\n}\n";
    let (_, printed) = run(source, &Options::default()).expect("runs");
    assert_eq!(printed, "hi\n");
}

/// Legacy syntax: an empty `when:` section and a slashy string among the inputs, which
/// Nextflow 26.04.6 ignores, running the shell script `echo 1`.
#[test]
fn accepts_empty_guards() {
    let source = "process P {\n    input:\n    val x\n    /tuple val(a), path(\"\") /\n    output:\n    stdout\n    \
                  when:\n\n    shell:\n    '''\n    echo !{x}\n    '''\n}\nworkflow {\n    P(1)\n}\n";
    let (tasks, _) = run(source, &Options::default()).expect("runs");
    assert_eq!(tasks[0].script, "#!/bin/bash -ue\necho 1\n");
}

/// As in Nextflow 26.04.6, an empty script or one with comments only does nothing, and a
/// script with named workflows only fails.
#[test]
fn executes_entryless_scripts() {
    let (tasks, printed) = run("// a placeholder\n", &Options::default()).expect("runs");
    assert!(tasks.is_empty() && printed.is_empty());
    let error = run(
        "workflow W {\n    main:\n    println(1)\n}\n",
        &Options::default(),
    )
    .unwrap_err();
    assert!(error.ends_with("No entry workflow specified"), "{error}");
}

/// Legacy syntax: a `def` before the first section of a process declares a variable that the
/// script reads, as Nextflow 26.04.6 runs pipeface's `mosdepth`.
#[test]
fn accepts_process_declarations() {
    let source = "process P {\n    def sw = 'mosdepth'\n    input:\n    val x\n    output:\n    stdout\n    \
                  script:\n    \"echo ${sw} ${x}\"\n}\nworkflow {\n    P(1)\n}\n";
    let (tasks, _) = run(source, &Options::default()).expect("runs");
    assert_eq!(tasks[0].script, "#!/bin/bash -ue\necho mosdepth 1\n");
}

/// Legacy syntax: `===` and `!==` test identity, and Groovy's `String.valueOf` and
/// `plus(index, element)`; Nextflow 26.04.6 prints `[true, true, false, null, 5, [1, 9, 2]]`.
#[test]
fn preserves_value_identity() {
    let source = "workflow {\n    def m = [a: 1]\n    \
                  println([m !== [a: 1], m === m, [1] === [1], String.valueOf(null), String.valueOf(5), [1, 2].plus(1, 9)])\n}\n";
    let (_, printed) = run(source, &Options::default()).expect("runs");
    assert_eq!(printed, "[true, true, false, null, 5, [1, 9, 2]]\n");
}

/// `java.text.NumberFormat` parses grouped numbers and `DateTimeFormatter` formats the start of
/// the run, as in pathogensurveillance and taxodactyl; Nextflow 26.04.6 prints
/// `[1234567, 12.5, -3, 15]`.
#[test]
fn formats_scalar_values() {
    let source = "workflow {\n    def nf = java.text.NumberFormat.getInstance(Locale.US)\n    \
                  def f = java.time.format.DateTimeFormatter.ofPattern('yyyyMMdd HHmmss').withZone(java.time.ZoneId.systemDefault())\n    \
                  println([nf.parse('1,234,567').intValue(), nf.parse('12.5x'), nf.parse('-3'), f.format(workflow.start).size()])\n}\n";
    let (_, printed) = run(source, &Options::default()).expect("runs");
    assert_eq!(printed, "[1234567, 12.5, -3, 15]\n");
}

/// Legacy syntax: the statements of a workflow before `emit:` are its main block, as in
/// assemblyeval; Nextflow 26.04.6 prints `2`.
#[test]
fn executes_unlabelled_statements() {
    let source = "workflow W {\n    x = channel.of(1)\n    y = x.map { it + 1 }\n    emit:\n    y\n    z = x\n}\n\
                  workflow {\n    W().y.view()\n}\n";
    let (_, printed) = run(source, &Options::default()).expect("runs");
    assert_eq!(printed, "2\n");
}

/// Legacy syntax: modifiers like `public static` before a function of the script, and
/// `final` declarations; Nextflow 26.04.6 prints `[5, true]`.
#[test]
fn accepts_final_declarations() {
    let source = "public static boolean hasExt(String s, String[] extn) {\n    return extn.any { s.endsWith(it) }\n}\n\
                  workflow {\n    final x = 5\n    println([x, hasExt('a.txt', ['.txt'] as String[])])\n}\n";
    let (_, printed) = run(source, &Options::default()).expect("runs");
    assert_eq!(printed, "[5, true]\n");
}

/// Legacy syntax: a second `script:` goes on with the first, as in pgsc_calc's
/// MATCH_COMBINE; Nextflow 26.04.6 runs `echo --a`.
#[test]
fn repeated_script_sections() {
    let source = "process P {\n    input:\n    val x\n    output:\n    stdout\n    script:\n    def ambig = x ? '--a' : ''\n\n    \
                  script:\n    if (x == 1)\n        \"echo ${ambig}\"\n    else\n        'echo no'\n}\nworkflow {\n    P(1)\n}\n";
    let (tasks, _) = run(source, &Options::default()).expect("runs");
    assert_eq!(tasks[0].script, "#!/bin/bash -ue\necho --a\n");
}

/// `.value` of a plain value that a workflow emits gives the value while the workflow is
/// built, as in spatialvpt's `report_only.value`; Nextflow 26.04.6 prints `false` and `no`.
#[test]
fn reads_plain_emits() {
    let source = "workflow W {\n    take:\n    flag\n    main:\n    println(flag)\n    emit:\n    flag = flag\n}\n\
                  workflow {\n    W(false)\n    def f = W.out.flag\n    if (f.value) { println('yes') } else { println('no') }\n}\n";
    let (_, printed) = run(source, &Options::default()).expect("runs");
    assert_eq!(printed, "false\nno\n");
}

/// Legacy syntax: an emit assignment sets its variable, which later emits read, as in
/// metabiomx's CHECK_INPUT; Nextflow 26.04.6 prints `2`.
#[test]
fn resolves_earlier_emits() {
    let source = "workflow W {\n    main:\n    ch = channel.of(1, 2)\n    emit:\n    meta = ch\n    n = meta.count()\n}\n\
                  workflow {\n    W().n.view()\n}\n";
    let (_, printed) = run(source, &Options::default()).expect("runs");
    assert_eq!(printed, "2\n");
}

/// Legacy syntax: a string among the directives of a process, like a description, does
/// nothing, as Nextflow 26.04.6 runs the `RENAME_FASTA_CONTIGS` of 2105.
#[test]
fn strings_among_directives() {
    let source = "process P {\n    '''\n    Some docs\n    '''\n    tag { x }\n    input:\n    val x\n    output:\n    stdout\n    \
                  script:\n    \"echo ${x}\"\n}\nworkflow {\n    P(1)\n}\n";
    let (tasks, _) = run(source, &Options::default()).expect("runs");
    assert_eq!(tasks[0].name, "P (1)");
}
