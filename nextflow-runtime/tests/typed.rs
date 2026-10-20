//! Typed processes, which `nextflow.enable.types = true` turns on, against a fake executor,
//! as Nextflow 26.04.6 runs them.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::interpreter::Host;
use nextflow_evaluator::lower::lower_module;
use nextflow_evaluator::value::PathValue;
use nextflow_parser::parse_script;
use nextflow_runtime::{Options, Run, TaskOutcome, TaskRequest};

#[derive(Default)]
struct Output(String);

impl Host for Output {
    fn print(&mut self, text: &str) {
        self.0.push_str(text);
    }
}

/// Runs a script, finishing every task in request order: each output pattern becomes a file
/// in `/work/<task>`, standard output is `out`, each environment variable `env` and each
/// `eval` command answers `1.13`. The result has the requests and what the workflow printed.
fn run(source: &str) -> Result<(Vec<TaskRequest>, String), String> {
    let parsed = parse_script(source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let module = lower_module(&parsed.tree, source).map_err(|errors| format!("{errors:?}"))?;
    let module = Rc::new(module);
    let mut output = Output::default();
    let (mut run, mut queue) =
        Run::start(&module, &Options::default(), &mut output).map_err(|error| error.to_string())?;
    let mut done = Vec::new();
    while !queue.is_empty() {
        let request = queue.remove(0);
        let folder = format!("/work/{}", request.id.0);
        let outcome = TaskOutcome {
            id: request.id,
            files: (request.output_patterns.iter())
                .map(|name| {
                    (
                        name.clone(),
                        PathValue::parse(&format!("{folder}/{name}"), "/"),
                    )
                })
                .collect(),
            stdout: "out".into(),
            env: (request.env.iter())
                .map(|name| (name.to_string(), "env".into()))
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

/// Typed inputs: a path, a collection of paths that is a plain list, values, a tuple and a
/// record, with the `stage:` section naming files and setting a variable and the input.
#[test]
fn binds_typed_inputs() {
    let source = r#"nextflow.enable.types = true

process S {
    input:
    first: Path
    many: Set<Path>
    name: String
    tuple(id: String, extra: Path)
    record(key: String, other: Path)

    stage:
    stageAs first, 'renamed.txt'
    stageAs many, 'm*.txt'
    env 'GREETING', name
    stdin name

    script:
    "echo ${first} ${many} ${many.getClass().simpleName} ${name} ${id} ${extra} ${key} ${other}"
}

workflow {
    S(file('/data/x.fq'), [file('/data/b.fq'), file('/data/a.fq')], 'hello', tuple('s1', file('/data/e.fq')), record(key: 'k', other: file('/data/o.fq')))
}
"#;
    let (tasks, _) = run(source).expect("runs");
    let [task] = tasks.as_slice() else {
        panic!("one task")
    };
    let script = task.script.lines().last().unwrap_or_default();
    assert_eq!(
        script,
        "echo renamed.txt [m1.txt, m2.txt] ArrayList hello s1 e.fq k o.fq"
    );
    let staged: Vec<&str> = task.staged.iter().map(|(_, name)| name.as_str()).collect();
    assert_eq!(staged, ["renamed.txt", "m1.txt", "m2.txt", "e.fq", "o.fq"]);
    assert_eq!(task.input_environment[0].0.as_ref(), "GREETING");
    assert_eq!(task.input_environment[0].1.to_string(), "hello");
    assert_eq!(
        task.stdin.as_ref().map(ToString::to_string).as_deref(),
        Some("hello")
    );
}

/// Without the feature flag, typed inputs fail as in Nextflow 26.04.6.
#[test]
fn needs_types_flag() {
    let source = "process S {\n    input:\n    name: String\n    script:\n    'true'\n}\nworkflow {\n    S('a')\n}\n";
    let error = run(source).unwrap_err();
    assert!(
        error.contains("Typed input declaration is not allowed in legacy process"),
        "{error}"
    );
}

/// Typed outputs compute their values after the task from `file()`, `files()`, `stdout()`,
/// `env()` and `eval()`, also inside tuples and records, and the `topic:` section feeds its
/// topic without being an output.
#[test]
fn computes_typed_outputs() {
    let source = r#"nextflow.enable.types = true

process GREET {
    input:
    name: String
    count: Integer

    output:
    text: Path = file('out.txt')
    n: Integer = count + 1
    msg = stdout()
    other = env('FOO')
    ev = eval('echo x')
    all = files('*.out')
    pair = tuple(name, file('out.txt'))
    rec = record(id: name, size: count)

    topic:
    tuple('GREET', eval('tool --version')) >> 'versions'

    script:
    "touch out.txt"
}

workflow {
    def r = GREET('hi', 2)
    r.text.subscribe { v -> println "text ${v.name}" }
    r.n.subscribe { v -> println "n ${v}" }
    r.msg.subscribe { v -> println "msg ${v}" }
    r.other.subscribe { v -> println "other ${v}" }
    r.ev.subscribe { v -> println "ev ${v}" }
    r.all.subscribe { v -> println "all ${v*.name} ${v.getClass().simpleName}" }
    r.pair.subscribe { v -> println "pair ${v[0]} ${v[1].name}" }
    r.rec.subscribe { v -> println "rec ${v}" }
    channel.topic('versions').subscribe { v -> println "topic ${v}" }
}
"#;
    let (tasks, printed) = run(source).expect("runs");
    assert_eq!(tasks.len(), 1);
    let mut lines: Vec<&str> = printed.lines().collect();
    lines.sort_unstable();
    assert_eq!(
        lines,
        [
            "all [*.out] LinkedHashSet",
            "ev 1.13",
            "msg out",
            "n 3",
            "other env",
            "pair hi out.txt",
            "rec [id:hi, size:2]",
            "text out.txt",
            "topic [GREET, 1.13]",
        ]
    );
}
