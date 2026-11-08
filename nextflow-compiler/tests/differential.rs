//! Runs small Nextflow pipelines natively and compiled on the simulator, in both completion
//! orders, and compares their tasks and resume fingerprints with the recorded counts.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use nextflow_runtime::driver::{Nextflow, Settings};
use serde_json::Value as Json;
use workflow_graph::{Driver, Launch, Order, RunOptions, Simulator, Task, execute_with};

/// How many pipelines compile fully, run natively, run compiled, and how many compiled runs give
/// the native tasks in both orders: equal task descriptions and resume fingerprints.
const RECORDED: Counts = Counts {
    pipelines: 11,
    full: 11,
    native: 11,
    compiled: 11,
    same: 11,
};

#[derive(Debug, Default, PartialEq)]
struct Counts {
    pipelines: usize,
    full: usize,
    native: usize,
    compiled: usize,
    same: usize,
}

const PROCESSES: &str = r#"
process GREET {
    tag "${meta.id}"
    memory { 1.GB * task.attempt }
    input:
    tuple val(meta), val(name)
    output:
    tuple val(meta), path("${name}.txt"), emit: text
    tuple val("${task.process}"), val('greet'), val('1.0'), topic: versions
    script:
    "echo ${params.greeting} ${name} > ${name}.txt"
}

process COUNT {
    input:
    path texts
    val label
    output:
    path 'count.txt'
    script:
    "cat ${texts} | wc -l > count.txt; echo ${label}"
}
"#;

/// The pipelines, each with its workflow body after the processes.
const PIPELINES: &[(&str, &str)] = &[
    (
        "map",
        "workflow { GREET(channel.of('a', 'b').map { [[id: it], it] }) }",
    ),
    (
        "collect",
        "workflow { COUNT(GREET(channel.of([[id: 'a'], 'a'], [[id: 'b'], 'b'])).text.map { it[1] }.collect(), 'all') }",
    ),
    (
        "branch",
        "workflow {\n    words = channel.of('a', 'bb', 'ccc').branch { w ->\n        small: w.size() < 2\n        large: true\n    }\n    GREET(words.large.map { [[id: it], it] })\n}",
    ),
    (
        "join",
        "workflow {\n    left = channel.of([[id: 'a'], 'a'], [[id: 'b'], 'b'])\n    GREET(left)\n    GREET.out.text.join(left).map { meta, text, name -> text }.collect().set { texts }\n    COUNT(texts, 'joined')\n}",
    ),
    (
        "group",
        "workflow {\n    GREET(channel.of(['x', 'a'], ['x', 'b'], ['y', 'c']).map { k, v -> [[id: v, group: k], v] })\n    GREET.out.text.map { meta, text -> [meta.group, text] }.groupTuple().map { it[1] }.flatMap { it }.collect().set { texts }\n    COUNT(texts, 'grouped')\n}",
    ),
    (
        "topic",
        "workflow {\n    GREET(channel.of([[id: 'a'], 'a']))\n    channel.topic('versions').map { it[1] }.unique().collect().view()\n}",
    ),
    (
        "subworkflow",
        "workflow GREETING {\n    take:\n    names\n    main:\n    GREET(names.map { [[id: it], it] })\n    emit:\n    texts = GREET.out.text.map { it[1] }\n}\n\nworkflow {\n    GREETING(channel.of('a', 'b', 'c'))\n    COUNT(GREETING.out.texts.collect(), 'sub')\n}",
    ),
    (
        "combine",
        "workflow {\n    GREET(channel.of('a', 'b').combine(channel.of(1, 2)).map { n, i -> [[id: \"${n}${i}\"], n] })\n    COUNT(GREET.out.text.map { it[1] }.toSortedList(), 'combined')\n}",
    ),
    (
        "files",
        "workflow {\n    GREET(channel.fromPath(\"${projectDir}/*.csv\").map { [[id: it.baseName], it.baseName] })\n    COUNT(channel.fromFilePairs(\"${projectDir}/*_{1,2}.fq\").map { it[1] }, 'pairs')\n}",
    ),
    (
        "optional",
        "process OPT {\n    input:\n    val x\n    output:\n    path 'a.txt', emit: a\n    path 'b.txt', optional: true, emit: b\n    script:\n    'touch a.txt'\n}\nworkflow {\n    OPT(channel.of(1, 2))\n    GREET(OPT.out.b.count().map { [[id: \"n${it}\"], \"n${it}\"] })\n}",
    ),
    (
        "closures",
        "def shout(word) { word.toUpperCase() }\nworkflow {\n    def keep = { w -> w != 'b' }\n    GREET(channel.of('a', 'b', 'c').map { [it] }.map { it.findAll(keep) }.filter { it }.map { [[id: shout(it[0]) + params.greeting], it[0]] })\n}",
    ),
];

/// A host in the folder of a run that prints nothing.
struct Quiet(String);

impl nextflow_evaluator::interpreter::Host for Quiet {
    fn print(&mut self, _: &str) {}

    fn launch_dir(&self) -> String {
        self.0.clone()
    }
}

fn settings(folder: &Path) -> Settings {
    Settings {
        host: Box::new(Quiet(folder.to_string_lossy().into_owned())),
        ..Settings::default()
    }
}

/// The tasks of a simulated run as the resume database and the task cache see them, sorted,
/// with the resume fingerprint of each; or the error of the run.
fn tasks(
    driver: &mut dyn Driver,
    folder: &Path,
    order: Order,
) -> Result<Vec<(Json, String)>, String> {
    let mut simulator = Simulator::new(folder.join(format!("simulator-{order:?}")));
    let options = RunOptions {
        max_pending: 4,
        order,
    };
    let execution = execute_with("pipeline", driver, &mut simulator, &options);
    let graph = execution.into_result().map_err(|error| error.to_string())?;
    let mut tasks: Vec<(Json, String)> = (graph.nodes.iter())
        .map(|node| {
            let task = Task {
                id: workflow_graph::TaskId(0),
                after: Vec::new(),
                ..node.task.clone()
            };
            let fingerprint = run_database::fingerprint(&task, &[]);
            (serde_json::to_value(&task).unwrap_or_default(), fingerprint)
        })
        .collect();
    tasks.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(tasks)
}

/// The tasks of a compiled document run through the Nextflow task adapter.
fn run_compiled(
    launch: &Launch,
    folder: &Path,
    order: Order,
) -> Result<Vec<(Json, String)>, String> {
    let mut adapters = wir_runtime::adapter::Adapters::default();
    let launched = folder.to_owned();
    let options = wir_runtime::nextflow::Options::new(move |_, _| settings(&launched));
    wir_runtime::nextflow::register(&mut adapters, options);
    let wir = wir_runtime::Settings {
        print: false,
        adapters,
        ..wir_runtime::Settings::default()
    };
    let mut driver = wir_runtime::WirDriver::new(launch, wir).map_err(|error| error.message)?;
    tasks(&mut driver, folder, order)
}

/// Compiles and runs one pipeline in a folder of its own.
fn compare(name: &str, script: &str, counts: &mut Counts) -> String {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    std::fs::write(folder.join("main.nf"), format!("{PROCESSES}\n{script}\n")).unwrap();
    std::fs::write(folder.join("nextflow.config"), "params.greeting = 'hi'\n").unwrap();
    for name in ["a.csv", "b.csv", "s_1.fq", "s_2.fq"] {
        std::fs::write(folder.join(name), name).unwrap();
    }
    let launch = |target: PathBuf| Launch {
        target,
        scratch: folder.join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    counts.pipelines += 1;
    let compiled =
        match nextflow_compiler::compile(&launch(folder.to_owned()), settings(folder), folder) {
            Ok(compiled) => compiled,
            Err(error) => return format!("{name}: does not compile: {error}"),
        };
    if let Some(kept) = compiled.kept.first() {
        return format!("{name}: partly ({}: {})", kept.path, kept.reason);
    }
    counts.full += 1;
    let document = folder.join("compiled.wir.json");
    let text = serde_json::to_string(&compiled.document).expect("serializes");
    std::fs::write(&document, text).expect("writes the document");
    let mut outcomes = Vec::new();
    for order in [Order::AsAsked, Order::Reversed] {
        let native = Nextflow::new(&launch(folder.to_owned()), settings(folder));
        let native = native.map_err(|error| error.message);
        let native = native.and_then(|mut driver| tasks(&mut driver, folder, order));
        let compiled = run_compiled(&launch(document.clone()), folder, order);
        outcomes.push((native, compiled));
    }
    if outcomes.iter().all(|(native, _)| native.is_ok()) {
        counts.native += 1;
    }
    if outcomes.iter().all(|(_, compiled)| compiled.is_ok()) {
        counts.compiled += 1;
    }
    let same = outcomes
        .iter()
        .all(|(native, compiled)| match (native, compiled) {
            (Ok(native), Ok(compiled)) => native == compiled,
            _ => false,
        });
    if same {
        counts.same += 1;
    }
    let (native, compiled) = &outcomes[0];
    match (native, compiled) {
        (Err(error), _) => format!("{name}: the native run fails: {error}"),
        (Ok(_), Err(error)) => format!("{name}: the compiled run fails: {}", first(error)),
        (Ok(native), Ok(compiled)) if !same => format!(
            "{name}: {} native tasks, {} compiled tasks, or other tasks",
            native.len(),
            compiled.len()
        ),
        (Ok(native), Ok(_)) => format!("{name}: same {} tasks", native.len()),
    }
}

fn first(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

#[test]
fn matches_recorded_counts() {
    let mut counts = Counts::default();
    let lines: Vec<String> = (PIPELINES.iter())
        .map(|(name, script)| compare(name, script, &mut counts))
        .collect();
    assert_eq!(counts, RECORDED, "the outcomes:\n{}", lines.join("\n"));
}
