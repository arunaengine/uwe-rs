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
    r.pair.subscribe { v -> println "pair ${v[0]} ${v[1].name} ${v.getClass().simpleName}" }
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
            "pair hi out.txt ArrayList",
            "rec [id:hi, size:2]",
            "text out.txt",
            "topic [GREET, 1.13]",
        ]
    );
}

/// A call of a typed process gives its one output channel, or a record of the output channels
/// by name, and `set` fails when it runs, as in Nextflow 26.04.6.
#[test]
fn shapes_typed_results() {
    let source = r#"nextflow.enable.types = true

process A {
    input:
    x: Integer
    output:
    y = x * 2
    script:
    'true'
}

process B {
    input:
    x: Integer
    output:
    y = x * 2
    z = x * 3
    script:
    'true'
}

workflow {
    A(channel.of(1)).subscribe { v -> println "a $v" }
    def b = B(channel.of(4))
    println "b ${b.keySet().sort()}"
    b.y.subscribe { v -> println "by $v" }
}
"#;
    let (_, printed) = run(source).expect("runs");
    let mut lines: Vec<&str> = printed.lines().collect();
    lines.sort_unstable();
    assert_eq!(lines, ["a 2", "b [y, z]", "by 8"]);
    let source =
        "nextflow.enable.types = true\nworkflow {\n    channel.of(1).set { x }\n    x.view()\n}\n";
    let error = run(source).unwrap_err();
    assert!(
        error.ends_with(
            "Operator `set` is not supported in typed workflows -- use an assignment instead"
        ),
        "{error}"
    );
}

/// The operators that typed workflows change: `collect` keeps lists whole, `groupBy` groups
/// by key, `join` pairs records by a field and `combine` spreads only tuples or adds fields.
#[test]
fn runs_typed_operators() {
    let source = r#"nextflow.enable.types = true

workflow {
    channel.of([1], [2]).collect().subscribe { v -> println "collect ${v.sort()}" }
    channel.of(record(id: 1, a: 'x'), record(id: 2, a: 'y')).join(channel.of(record(id: 1, b: 'p')), by: 'id').subscribe { v -> println "join $v" }
    channel.of(record(id: 1)).combine(n: 5).subscribe { v -> println "combine named $v" }
    channel.of(1, [2]).combine(channel.of(tuple('a', 'b'))).subscribe { v -> println "combine $v" }
    channel.of(tuple('k', 1), tuple('k', 2), tuple('j', 3)).groupBy().subscribe { v -> println "groupBy ${v[0]} ${v[1].sort()}" }
    channel.of(tuple('s', 2, 1), tuple('s', 2, 2)).groupBy().subscribe { v -> println "sized ${v[0]} ${v[1].sort()}" }
    channel.value(5).map { v -> v + 1 }.subscribe { v -> println "vmap $v" }
}
"#;
    let (_, printed) = run(source).expect("runs");
    let mut lines: Vec<&str> = printed.lines().collect();
    lines.sort_unstable();
    assert_eq!(
        lines,
        [
            "collect [[1], [2]]",
            "combine [1, a, b]",
            "combine [[2], a, b]",
            "combine named [id:1, n:5]",
            "groupBy j [3]",
            "groupBy k [1, 2]",
            "join [id:1, a:x, b:p]",
            "sized s [1, 2]",
            "vmap 6",
        ]
    );
}

/// A typed workflow with several emits gives a record of channels, and channels are
/// `ChannelImpl` and `ValueImpl`, as in Nextflow 26.04.6.
#[test]
fn shapes_workflow_results() {
    let source = r#"nextflow.enable.types = true

process P {
    input:
    x: Integer
    output:
    y = x + 1
    script:
    'true'
}

workflow SUB {
    take:
    xs: Channel<Integer>

    main:
    ys = P(xs)

    emit:
    ys: Channel<Integer> = ys
    total = ys.reduce { a, b -> a + b }
}

workflow {
    def r = SUB(channel.of(1, 2))
    r.ys.subscribe { v -> println "ys $v" }
    r.total.subscribe { v -> println "total $v" }
    println "r ${r.getClass().simpleName}"
    println "classes ${channel.of(1).getClass().name} ${channel.value(1).getClass().name}"
    channel.value([1, 2]).flatMap().subscribe { v -> println "flat $v" }
}
"#;
    let (_, printed) = run(source).expect("runs");
    let mut lines: Vec<&str> = printed.lines().collect();
    lines.sort_unstable();
    assert_eq!(
        lines,
        [
            "classes nextflow.dataflow.ChannelImpl nextflow.dataflow.ValueImpl",
            "flat 1",
            "flat 2",
            "r RecordMap",
            "total 5",
            "ys 2",
            "ys 3",
        ]
    );
}

/// A typed input takes null only with `?` on its type, and `reduce` of typed workflows fails
/// for an empty channel, as in Nextflow 26.04.6.
#[test]
fn checks_typed_nulls() {
    let script = |body: &str| {
        format!(
            "nextflow.enable.types = true\nprocess P {{\n    input:\n    x: Integer\n    output:\n    \
             y = x\n    script:\n    'true'\n}}\nprocess Q {{\n    input:\n    x: String?\n    output:\n    \
             y = x\n    script:\n    'true'\n}}\nworkflow {{\n    {body}\n}}\n"
        )
    };
    let error = run(&script("P(null)")).unwrap_err();
    assert!(
        error.ends_with(
            "[P] input at index 0 cannot be null -- append `?` to the type annotation to mark it \
             as nullable"
        ),
        "{error}"
    );
    let (tasks, _) = run(&script("Q(null)")).expect("runs");
    assert_eq!(tasks.len(), 1);
    let error = run(&script(
        "channel.empty().reduce { a, b -> a + b }.subscribe { v -> println v }",
    ))
    .unwrap_err();
    assert!(
        error.ends_with(
            "Operator `reduce` received an empty channel with no initial value -- make sure to \
             provide an initial value if the channel might be empty"
        ),
        "{error}"
    );
}

/// Runs `main.nf` with an included `sub.nf` and gives the printed lines, sorted.
fn run_pair(main: &str, sub: &str) -> Vec<String> {
    let root = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(root.path().join("main.nf"), main).expect("writes main.nf");
    std::fs::write(root.path().join("sub.nf"), sub).expect("writes sub.nf");
    let module = nextflow_runtime::Loader::default()
        .load(&root.path().join("main.nf"))
        .expect("loads");
    let mut output = Output::default();
    nextflow_runtime::run(&module, &mut output).expect("runs");
    let mut lines: Vec<String> = output.0.lines().map(str::to_owned).collect();
    lines.sort_unstable();
    lines
}

/// `nextflow.enable.types` types the operators of its own script only, also for channels
/// that come from the other script, as Nextflow 26.04.6 prints for these scripts.
#[test]
fn types_per_script() {
    let lines = run_pair(
        "nextflow.enable.types = true
include { SUB } from './sub'
workflow {
    SUB(channel.of([1, [2, 3]], [4]))
    channel.of([1, [2, 3]], [4]).collect().subscribe { v -> println \"main collect $v\" }
    channel.of(1).combine(channel.of('a')).subscribe { v -> println \"main combine $v\" }
    println \"main class ${channel.of(1).getClass().name}\"
}
",
        "workflow SUB {
    take:
    ch
    main:
    ch.collect().subscribe { v -> println \"sub collect $v\" }
    channel.of([1]).combine(channel.of(['a'])).subscribe { v -> println \"sub combine $v\" }
    println \"sub class ${channel.of(1).getClass().name}\"
    channel.of(5).set { x }
    x.tap { y }.subscribe { v -> println \"sub set $v\" }
}
",
    );
    assert_eq!(
        lines,
        [
            "main class nextflow.dataflow.ChannelImpl",
            "main collect [[1, [2, 3]], [4]]",
            "main combine [1, a]",
            "sub class groovyx.gpars.dataflow.DataflowBroadcast",
            "sub collect [1, [2, 3], 4]",
            "sub combine [1, a]",
            "sub set 5",
        ]
    );
    let lines = run_pair(
        "include { SUB } from './sub'
workflow {
    SUB(channel.of([1, [2, 3]], [4]))
    channel.of([1, [2, 3]], [4]).collect().subscribe { v -> println \"main collect $v\" }
}
",
        "nextflow.enable.types = true
workflow SUB {
    take:
    ch
    main:
    ch.collect().subscribe { v -> println \"sub collect $v\" }
    channel.of([1]).combine(channel.of(['a'])).subscribe { v -> println \"sub combine $v\" }
}
",
    );
    assert_eq!(
        lines,
        [
            "main collect [1, [2, 3], 4]",
            "sub collect [[1, [2, 3]], [4]]",
            "sub combine [[1], [a]]",
        ]
    );
}
