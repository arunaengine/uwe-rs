//! Runs demo CWL documents natively and through a wir document with one native op, and
//! compares the cache keys, the resume fingerprints and the outputs of their tasks.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use run_database::Database;
use serde_json::{Value as Json, json};
use workflow_graph::{Reuse, Task};

mod common;

/// Runs the binary in `directory` with the cache folder `cache`, without network for its
/// tasks, so that they may share stored results; its exit code and what it printed.
fn uwe(directory: &Path, cache: &Path, arguments: &[&str]) -> (i32, String) {
    let output = common::command()
        .args(arguments)
        .arg("-no-network")
        .current_dir(directory)
        .env("XDG_CACHE_HOME", cache)
        .output()
        .expect("runs uwe");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    (
        output.status.code().unwrap_or(-1),
        format!("{stdout}{stderr}"),
    )
}

/// A copy of the CWL demos in `root`, so that no run writes into the checkout.
fn demos(root: &Path) {
    let demos = Path::new(env!("CARGO_MANIFEST_DIR")).join("../demo/cwl");
    for name in [
        "scatter-workflow.cwl",
        "hello_world.cwl",
        "scatter-job.yml",
        "directory-copy.cwl",
    ] {
        std::fs::copy(demos.join(name), root.join(name)).expect("copies the demo");
    }
}

/// A wir document in `root` whose one native op `block` runs the CWL document `source`,
/// with the same ports on the graph and the op, and its inputs file.
fn wir(root: &Path, source: &str, ports: (Json, Json), inputs: Json) -> (PathBuf, PathBuf) {
    let (taken, given) = ports;
    let edges: Vec<Json> = (taken.as_array().unwrap().iter())
        .map(|port| json!({"from": {"port": port["name"]}, "to": {"op": "block", "port": port["name"]}}))
        .chain(given.as_array().unwrap().iter().map(|port| {
            json!({"from": {"op": "block", "port": port["name"]}, "to": {"port": port["name"]}})
        }))
        .collect();
    let document = json!({
        "wir_version": "0.1",
        "sources": [{"id": "block", "language": "cwl", "location": source}],
        "graphs": [{
            "name": "main", "inputs": taken, "outputs": given,
            "ops": [{
                "path": "block", "kind": {"native": {"source": "block", "language": "cwl"}},
                "inputs": taken, "outputs": given
            }],
            "edges": edges
        }],
        "entry": "main"
    });
    let (target, values) = (root.join("run.wir.json"), root.join("inputs.json"));
    std::fs::write(&target, document.to_string()).expect("writes the document");
    std::fs::write(&values, inputs.to_string()).expect("writes the inputs");
    (target, values)
}

/// The scatter demo as a wir document.
fn scatter(root: &Path) -> (PathBuf, PathBuf) {
    let port = json!([{"name": "message_array", "type": {"array": {"items": "string"}}}]);
    let messages = [
        "Hello world!",
        "Hola mundo!",
        "Bonjour le monde!",
        "Hallo welt!",
    ];
    wir(
        root,
        "scatter-workflow.cwl",
        (port, json!([])),
        json!({"message_array": messages}),
    )
}

/// A task of a recorded run, with its fingerprint and where its result came from.
struct Recorded {
    task: Task,
    fingerprint: String,
    reused: Option<String>,
}

/// The latest run in the run database: its id, its language and its tasks in the order
/// they were asked for.
fn latest(cache: &Path) -> (String, String, Vec<Recorded>) {
    let database = Database::open(&cache.join("uwe/runs.sqlite")).expect("opens");
    let run = database.runs(1).expect("reads").remove(0);
    let mut tasks = Vec::new();
    for row in database.tasks(&run.id).expect("reads the tasks") {
        let detail = database
            .task(&run.id, row.id)
            .expect("reads")
            .expect("a task");
        tasks.push(Recorded {
            task: detail.description,
            fingerprint: detail.fingerprint,
            reused: row.reused,
        });
    }
    (run.id, run.language, tasks)
}

/// The cache key of a task with the inputs it names, as the executors compute it.
fn key(task: &Task) -> String {
    let request = task_runner::request(task, task.image.clone(), Vec::new(), None);
    request.spec.cache_key().expect("a valid task").to_string()
}

/// Checks that two runs asked for the same tasks: equal steps, cache keys and fingerprints.
fn same_tasks(native: &[Recorded], through: &[Recorded]) {
    assert!(!native.is_empty());
    assert_eq!(native.len(), through.len());
    for (one, other) in native.iter().zip(through) {
        let name = &one.task.name;
        assert_eq!(one.task.step, other.task.step);
        assert_eq!(other.task.name, format!("block: {name}"));
        assert_eq!(key(&one.task), key(&other.task), "cache key of {name}");
        assert_eq!(one.fingerprint, other.fingerprint, "fingerprint of {name}");
    }
}

#[test]
fn matches_native_runs() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let (root, cache) = (folder.path(), folder.path().join("cache"));
    demos(root);
    let native = [
        "run",
        "-executor",
        "simulator",
        "scatter-workflow.cwl",
        "-inputs",
        "scatter-job.yml",
    ];
    let (code, output) = uwe(root, &cache, &native);
    assert_eq!(code, 0, "{output}");
    let (_, language, alone) = latest(&cache);
    assert_eq!((language.as_str(), alone.len()), ("cwl", 4));
    let (target, inputs) = scatter(root);
    let (target, inputs) = (target.to_string_lossy(), inputs.to_string_lossy());
    let through = ["run", "-executor", "simulator", &target, "-inputs", &inputs];
    let (code, output) = uwe(root, &cache, &through);
    assert_eq!(code, 0, "{output}");
    let (first, language, tasks) = latest(&cache);
    assert_eq!(language, "wir", "the run is recorded as a wir run");
    same_tasks(&alone, &tasks);
    // A resumed wir run takes every task from the run it resumes.
    let resumed: Vec<&str> = through.iter().copied().chain(["-resume"]).collect();
    let (code, output) = uwe(root, &cache, &resumed);
    assert_eq!(code, 0, "{output}");
    let (_, _, again) = latest(&cache);
    same_tasks(&alone, &again);
    for recorded in &again {
        let name = &recorded.task.name;
        assert_eq!(
            recorded.reused.as_deref(),
            Some(first.as_str()),
            "{name} is resumed"
        );
    }
}

#[test]
fn skips_cache_steps() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let (root, cache) = (folder.path(), folder.path().join("cache"));
    demos(root);
    let native = [
        "run",
        "-executor",
        "simulator",
        "scatter-workflow.cwl",
        "-inputs",
        "scatter-job.yml",
    ];
    let skip = ["-no-cache-steps", "echo"];
    let words: Vec<&str> = native.iter().chain(&skip).copied().collect();
    let (code, output) = uwe(root, &cache, &words);
    assert_eq!(code, 0, "{output}");
    let (_, _, alone) = latest(&cache);
    let (target, inputs) = scatter(root);
    let (target, inputs) = (target.to_string_lossy(), inputs.to_string_lossy());
    let through = ["run", "-executor", "simulator", &target, "-inputs", &inputs];
    let words: Vec<&str> = through.iter().chain(&skip).copied().collect();
    let (code, output) = uwe(root, &cache, &words);
    assert_eq!(code, 0, "{output}");
    let (_, _, tasks) = latest(&cache);
    same_tasks(&alone, &tasks);
    for recorded in &tasks {
        let task = &recorded.task;
        assert_eq!(task.reuse, Reuse::Disabled, "{} uses no cache", task.name);
    }
}

/// The text of each file below a folder, by its path there.
fn texts(folder: &Path) -> Vec<(String, String)> {
    let mut texts: Vec<(String, String)> = std::fs::read_dir(folder)
        .expect("lists the folder")
        .flatten()
        .filter(|entry| entry.path().is_file())
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            (name, std::fs::read_to_string(entry.path()).expect("reads"))
        })
        .collect();
    texts.sort();
    texts
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn matches_native_outputs() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let (root, cache) = (folder.path(), folder.path().join("cache"));
    demos(root);
    std::fs::create_dir(root.join("data")).expect("makes the data folder");
    std::fs::write(root.join("data/a.txt"), "one\n").expect("writes");
    std::fs::write(root.join("data/b.txt"), "two two\n").expect("writes");
    let job = json!({"folder": {"class": "Directory", "location": "data"}});
    std::fs::write(root.join("job.json"), job.to_string()).expect("writes the job");
    let native = [
        "run",
        "directory-copy.cwl",
        "-inputs",
        "job.json",
        "-output-dir",
        "native",
    ];
    let (code, output) = uwe(root, &cache, &native);
    assert_eq!(code, 0, "{output}");
    let (_, _, alone) = latest(&cache);
    let ports = (
        json!([{"name": "folder", "type": "directory"}]),
        json!([{"name": "copies", "type": {"array": {"items": {"file": {}}}}}]),
    );
    let (target, inputs) = wir(root, "directory-copy.cwl", ports, job);
    let (target, inputs) = (target.to_string_lossy(), inputs.to_string_lossy());
    let through = ["run", &target, "-inputs", &inputs, "-output-dir", "wir"];
    let (code, output) = uwe(root, &cache, &through);
    assert_eq!(code, 0, "{output}");
    let (_, _, tasks) = latest(&cache);
    same_tasks(&alone, &tasks);
    let reused = tasks[0].reused.as_deref();
    assert_eq!(
        reused,
        Some("cache"),
        "the wir run reuses the stored result"
    );
    let delivered = texts(&root.join("native"));
    assert_eq!(delivered.len(), 2, "{delivered:?}");
    assert_eq!(texts(&root.join("wir/block")), delivered);
}

/// `wir/examples/two-tools.json` in `root`, with tasks in `image`, and its inputs file.
fn two_tools(root: &Path, image: &str) -> (String, String) {
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../wir/examples/two-tools.json");
    let mut document: Json =
        serde_json::from_str(&std::fs::read_to_string(example).expect("reads")).expect("JSON");
    for op in document["graphs"][0]["ops"].as_array_mut().expect("ops") {
        op["kind"]["task"]["image"] = json!(image);
    }
    let format = "http://edamontology.org/format_2330";
    let text = json!({"class": "File", "location": "text.txt", "format": format});
    std::fs::write(root.join("text.txt"), "b\nc\na\n").expect("writes the text");
    let inputs = root.join("inputs.json");
    std::fs::write(&inputs, json!({"text": text, "lines": 2}).to_string()).expect("writes");
    let target = root.join("two-tools.json");
    std::fs::write(&target, document.to_string()).expect("writes the document");
    let shown = |path: PathBuf| path.to_string_lossy().into_owned();
    (shown(target), shown(inputs))
}

#[test]
fn records_task_ops() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let (root, cache) = (folder.path(), folder.path().join("cache"));
    let (target, inputs) = two_tools(root, "debian:stable-slim");
    let words = ["run", "-executor", "simulator", &target, "-inputs", &inputs];
    let (code, output) = uwe(root, &cache, &words);
    assert_eq!(code, 0, "{output}");
    let (first, language, tasks) = latest(&cache);
    assert_eq!(language, "wir");
    let steps: Vec<&str> = tasks.iter().map(|item| item.task.step.as_str()).collect();
    assert_eq!(steps, ["sort", "head"]);
    let resumed: Vec<&str> = words.iter().copied().chain(["-resume"]).collect();
    let (code, output) = uwe(root, &cache, &resumed);
    assert_eq!(code, 0, "{output}");
    let (_, _, again) = latest(&cache);
    for (one, other) in tasks.iter().zip(&again) {
        assert_eq!(one.fingerprint, other.fingerprint);
        assert_eq!(
            other.reused.as_deref(),
            Some(first.as_str()),
            "{}",
            other.task.name
        );
    }
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn runs_task_ops() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let (root, cache) = (folder.path(), folder.path().join("cache"));
    let (target, inputs) = two_tools(root, "debian:stable-slim");
    let words = ["run", &target, "-inputs", &inputs, "-output-dir", "out"];
    let (code, output) = uwe(root, &cache, &words);
    assert_eq!(code, 0, "{output}");
    let top = std::fs::read_to_string(root.join("out/top.stdout")).expect("the delivered output");
    assert_eq!(top, "a\nb\n");
    let (first, _, tasks) = latest(&cache);
    assert!(
        tasks.iter().all(|item| item.reused.is_none()),
        "the first run runs every task"
    );
    let (code, output) = uwe(root, &cache, &words);
    assert_eq!(code, 0, "{output}");
    let (_, _, again) = latest(&cache);
    for item in &again {
        assert_eq!(
            item.reused.as_deref(),
            Some("cache"),
            "{} is cached",
            item.task.name
        );
    }
    // A real run resumes only results of digest-pinned images; others come from the cache.
    let resumed: Vec<&str> = words.iter().copied().chain(["-resume", &first]).collect();
    let (code, output) = uwe(root, &cache, &resumed);
    assert_eq!(code, 0, "{output}");
    let (_, _, last) = latest(&cache);
    assert!(
        last.iter().all(|item| item.reused.is_some()),
        "nothing runs again"
    );
}

/// The greetings pipeline of wir-runtime in `root`, a wir document that runs its workflow
/// `greetings` as the native op `block`, and the inputs of both runs.
fn greetings(root: &Path, names: &[&str]) -> (String, String) {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../wir-runtime/tests/fixtures/greetings.nf");
    std::fs::copy(fixture, root.join("greetings.nf")).expect("copies the pipeline");
    let values = json!({"names": names, "outdir": "results"});
    let inputs = root.join("params.json");
    std::fs::write(&inputs, values.to_string()).expect("writes the params");
    let taken = json!([
        {"name": "names", "type": {"array": {"items": "string"}}},
        {"name": "outdir", "type": "string"}
    ]);
    let given = json!([
        {"name": "files", "type": {"array": {"items": {"file": {}}}}},
        {"name": "outdir", "type": "directory"}
    ]);
    let edges: Vec<Json> = (taken.as_array().unwrap().iter())
        .map(|port| json!({"from": {"port": port["name"]}, "to": {"op": "block", "port": port["name"]}}))
        .chain(given.as_array().unwrap().iter().map(|port| {
            json!({"from": {"op": "block", "port": port["name"]}, "to": {"port": port["name"]}})
        }))
        .collect();
    let native = json!({"source": "block", "language": "nextflow", "entry": "greetings"});
    let document = json!({
        "wir_version": "0.2",
        "sources": [{"id": "block", "language": "nextflow", "location": "greetings.nf"}],
        "graphs": [{
            "name": "main", "inputs": taken, "outputs": given,
            "ops": [{"path": "block", "kind": {"native": native}, "inputs": taken, "outputs": given}],
            "edges": edges
        }],
        "entry": "main"
    });
    let target = root.join("run.wir.json");
    std::fs::write(&target, document.to_string()).expect("writes the document");
    let shown = |path: PathBuf| path.to_string_lossy().into_owned();
    (shown(target), shown(inputs))
}

#[test]
fn matches_native_nextflow() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let (root, cache) = (folder.path(), folder.path().join("cache"));
    let (target, inputs) = greetings(root, &["ada", "bob"]);
    // The native run starts in the folder that the block is launched in.
    let launch = root.join("block");
    std::fs::create_dir(&launch).expect("makes the launch folder");
    let script = root.join("greetings.nf");
    let script = script.to_string_lossy();
    let native = [
        "run",
        "-executor",
        "simulator",
        &script,
        "-entry",
        "greetings",
        "-params-file",
        &inputs,
    ];
    let (code, output) = uwe(&launch, &cache, &native);
    assert_eq!(code, 0, "{output}");
    let (_, language, alone) = latest(&cache);
    assert_eq!((language.as_str(), alone.len()), ("nextflow", 2));
    let through = ["run", "-executor", "simulator", &target, "-inputs", &inputs];
    let (code, output) = uwe(root, &cache, &through);
    assert_eq!(code, 0, "{output}");
    let (first, language, tasks) = latest(&cache);
    assert_eq!(language, "wir");
    same_tasks(&alone, &tasks);
    let resumed: Vec<&str> = through.iter().copied().chain(["-resume"]).collect();
    let (code, output) = uwe(root, &cache, &resumed);
    assert_eq!(code, 0, "{output}");
    let (_, _, again) = latest(&cache);
    same_tasks(&alone, &again);
    for recorded in &again {
        let name = &recorded.task.name;
        assert_eq!(
            recorded.reused.as_deref(),
            Some(first.as_str()),
            "{name} is resumed"
        );
    }
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn matches_nextflow_outputs() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let (root, cache) = (folder.path(), folder.path().join("cache"));
    let (target, inputs) = greetings(root, &["ada", "bob"]);
    let launch = root.join("native");
    std::fs::create_dir(&launch).expect("makes the launch folder");
    let script = root.join("greetings.nf");
    let script = script.to_string_lossy();
    let native = [
        "run",
        &script,
        "-entry",
        "greetings",
        "-params-file",
        &inputs,
    ];
    let (code, output) = uwe(&launch, &cache, &native);
    assert_eq!(code, 0, "{output}");
    let (_, _, alone) = latest(&cache);
    let through = ["run", &target, "-inputs", &inputs, "-output-dir", "wir"];
    let (code, output) = uwe(root, &cache, &through);
    assert_eq!(code, 0, "{output}");
    let (_, _, tasks) = latest(&cache);
    same_tasks(&alone, &tasks);
    let delivered = texts(&launch.join("results/greetings"));
    assert_eq!(delivered.len(), 2, "{delivered:?}");
    assert_eq!(delivered[0].1, "hello ada\n");
    assert_eq!(texts(&root.join("wir/block/results/greetings")), delivered);
    // The emitted files reach the output folder of the run, and the folder input stays.
    assert_eq!(texts(&root.join("wir")), delivered, "{output}");
    let start = output.find('{').expect("the outputs");
    let mut values = serde_json::Deserializer::from_str(&output[start..]).into_iter::<Json>();
    let printed = values
        .next()
        .expect("a value")
        .expect("the outputs as JSON");
    let published = root.join("wir/block/results");
    assert_eq!(
        printed["outdir"]["location"],
        published.to_string_lossy().as_ref()
    );
}

/// The greetings Snakefile of wir-runtime in `root`, a wir document that runs it as the native
/// op `block`, and the inputs of both runs.
fn snakefile(root: &Path, names: &[&str]) -> (String, String) {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../wir-runtime/tests/fixtures/greetings.smk");
    std::fs::copy(fixture, root.join("greetings.smk")).expect("copies the Snakefile");
    let inputs = root.join("config.json");
    std::fs::write(&inputs, json!({"names": names}).to_string()).expect("writes the config");
    let taken = json!([{"name": "names", "type": {"array": {"items": "string"}}}]);
    let given = json!([
        {"name": "greetings", "type": {"array": {"items": {"file": {}}}}},
        {"name": "total", "type": {"file": {}}}
    ]);
    let edges: Vec<Json> = (taken.as_array().unwrap().iter())
        .map(|port| json!({"from": {"port": port["name"]}, "to": {"op": "block", "port": port["name"]}}))
        .chain(given.as_array().unwrap().iter().map(|port| {
            json!({"from": {"op": "block", "port": port["name"]}, "to": {"port": port["name"]}})
        }))
        .collect();
    let native = json!({"source": "block", "language": "snakemake"});
    let document = json!({
        "wir_version": "0.2",
        "sources": [{"id": "block", "language": "snakemake", "location": "greetings.smk"}],
        "graphs": [{
            "name": "main", "inputs": taken, "outputs": given,
            "ops": [{"path": "block", "kind": {"native": native}, "inputs": taken, "outputs": given}],
            "edges": edges
        }],
        "entry": "main"
    });
    let target = root.join("run.wir.json");
    std::fs::write(&target, document.to_string()).expect("writes the document");
    let shown = |path: PathBuf| path.to_string_lossy().into_owned();
    (shown(target), shown(inputs))
}

#[test]
fn matches_native_snakemake() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let (root, cache) = (folder.path(), folder.path().join("cache"));
    let (target, inputs) = snakefile(root, &["ada", "bob"]);
    // The native run starts in the folder that the block starts in.
    let launch = root.join("block");
    std::fs::create_dir(&launch).expect("makes the folder");
    let script = root.join("greetings.smk");
    let script = script.to_string_lossy();
    let native = ["run", "-executor", "simulator", &script, "-inputs", &inputs];
    let (code, output) = uwe(&launch, &cache, &native);
    assert_eq!(code, 0, "{output}");
    let (_, language, alone) = latest(&cache);
    assert_eq!((language.as_str(), alone.len()), ("snakemake", 4));
    let through = ["run", "-executor", "simulator", &target, "-inputs", &inputs];
    let (code, output) = uwe(root, &cache, &through);
    assert_eq!(code, 0, "{output}");
    let (_, language, tasks) = latest(&cache);
    assert_eq!(language, "wir");
    same_inputs(&alone, &tasks);
    // A block resumes as the Snakefile does alone: with the same tasks taken from earlier.
    let resumed: Vec<&str> = native.iter().copied().chain(["-resume"]).collect();
    let (code, output) = uwe(&launch, &cache, &resumed);
    assert_eq!(code, 0, "{output}");
    let (_, _, alone) = latest(&cache);
    let resumed: Vec<&str> = through.iter().copied().chain(["-resume"]).collect();
    let (code, output) = uwe(root, &cache, &resumed);
    assert_eq!(code, 0, "{output}");
    let (_, _, again) = latest(&cache);
    assert_eq!(alone.len(), again.len());
    for (one, other) in alone.iter().zip(&again) {
        let name = &other.task.name;
        assert_eq!(one.reused.is_some(), other.reused.is_some(), "{name}");
    }
}

/// Checks that two runs asked for the same tasks, with equal steps, cache keys and fingerprints
/// for the same input files: each run makes the files of its tasks anew, and a simulated run
/// keeps them in a folder of its own, so inputs count as equal when they are equal there.
fn same_inputs(native: &[Recorded], through: &[Recorded]) {
    assert!(!native.is_empty());
    assert_eq!(native.len(), through.len());
    for (one, other) in native.iter().zip(through) {
        let name = &one.task.name;
        assert_eq!(one.task.step, other.task.step);
        assert_eq!(other.task.name, format!("block: {name}"));
        // A rule without a command, like `all`, asks for its inputs and has no cache key.
        if !one.task.command.is_empty() {
            assert_eq!(key(&one.task), key(&other.task), "cache key of {name}");
        }
        let mut placed = other.task.clone();
        for (input, alone) in placed.inputs.iter_mut().zip(&one.task.inputs) {
            let within =
                |source: &str| source.split_once("/work/").map(|(_, rest)| rest.to_owned());
            if within(&input.source).is_some() && within(&input.source) == within(&alone.source) {
                input.source = alone.source.clone();
            }
        }
        let states = run_database::input_states(&one.task);
        let alone = run_database::fingerprint(&one.task, &states);
        assert_eq!(alone, run_database::fingerprint(&placed, &states), "{name}");
        if one.task.inputs.is_empty() {
            assert_eq!(one.fingerprint, other.fingerprint, "fingerprint of {name}");
        }
    }
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn matches_snakemake_outputs() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let (root, cache) = (folder.path(), folder.path().join("cache"));
    let (target, inputs) = snakefile(root, &["ada", "bob"]);
    // Local jobs run in the working folder itself, which their tasks name; so the native run
    // works in the folder of the block, and its results move aside before the block runs.
    let launch = root.join("block");
    std::fs::create_dir(&launch).expect("makes the folder");
    let script = root.join("greetings.smk");
    let script = script.to_string_lossy();
    let (code, output) = uwe(&launch, &cache, &["run", &script, "-inputs", &inputs]);
    assert_eq!(code, 0, "{output}");
    let (_, _, alone) = latest(&cache);
    let native = root.join("native");
    std::fs::rename(&launch, &native).expect("moves the results aside");
    let (code, output) = uwe(root, &cache, &["run", &target, "-inputs", &inputs]);
    assert_eq!(code, 0, "{output}");
    let (_, _, tasks) = latest(&cache);
    same_inputs(&alone, &tasks);
    let delivered = texts(&native.join("greetings"));
    assert_eq!(delivered.len(), 2, "{delivered:?}");
    assert_eq!(delivered[0].1, "hello ada\n");
    assert_eq!(texts(&launch.join("greetings")), delivered);
    assert_eq!(texts(&launch), texts(&native), "the total");
    let start = output.find('{').expect("the outputs");
    let mut values = serde_json::Deserializer::from_str(&output[start..]).into_iter::<Json>();
    let printed = values
        .next()
        .expect("a value")
        .expect("the outputs as JSON");
    let made = launch.join("total.txt");
    assert_eq!(
        printed["total"]["location"],
        made.to_string_lossy().as_ref()
    );
}
