//! Runs the CWL fixtures of cwl-lowering natively and through their lowered wir documents, and
//! compares their steps, their tasks and their outputs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use run_database::Database;
use serde_json::{Value as Json, json};
use workflow_graph::Task;

mod common;

/// Runs the binary in `directory` with the cache folder `cache`; its exit code, what it
/// printed on standard output, and on standard error.
fn uwe(directory: &Path, cache: &Path, arguments: &[&str]) -> (i32, String, String) {
    let output = common::command()
        .args(arguments)
        .current_dir(directory)
        .env("XDG_CACHE_HOME", cache)
        .output()
        .expect("runs uwe");
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    let code = output.status.code().unwrap_or(-1);
    (code, text(&output.stdout), text(&output.stderr))
}

/// The fixtures in `root`, with a job for each and a text file.
fn fixtures(root: &Path) {
    let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("../cwl-lowering/tests/fixtures");
    for entry in std::fs::read_dir(folder).expect("lists the fixtures") {
        let path = entry.expect("an entry").path();
        std::fs::copy(&path, root.join(path.file_name().unwrap())).expect("copies");
    }
    std::fs::write(root.join("text.txt"), "b\na\n").expect("writes the text");
    let text = json!({"class": "File", "location": "text.txt"});
    std::fs::write(root.join("count.json"), json!({"text": text}).to_string()).expect("writes");
    std::fs::write(root.join("message.yml"), "message: hello\n").expect("writes the job");
    let messages = json!({"messages": ["a", "b", "c"]});
    std::fs::write(root.join("messages.json"), messages.to_string()).expect("writes the job");
    let lists = json!({"messages": ["a", "b"], "lists": [["x"], ["y", "z"]]});
    std::fs::write(root.join("lists.json"), lists.to_string()).expect("writes the job");
    for (name, go) in [("open.json", true), ("shut.json", false)] {
        let gate = json!({"message": "hello", "go": go});
        std::fs::write(root.join(name), gate.to_string()).expect("writes the job");
    }
}

/// The language of the latest run and its tasks with their fingerprints, by step.
fn latest(cache: &Path) -> (String, Vec<(Task, String, Option<String>)>) {
    let database = Database::open(&cache.join("uwe/runs.sqlite")).expect("opens");
    let run = database.runs(1).expect("reads").remove(0);
    let mut tasks = Vec::new();
    for row in database.tasks(&run.id).expect("reads the tasks") {
        let detail = (database.task(&run.id, row.id).expect("reads")).expect("a task");
        tasks.push((detail.description, detail.fingerprint, row.reused));
    }
    tasks.sort_by(|a, b| a.0.step.cmp(&b.0.step));
    (run.language, tasks)
}

fn key(task: &Task) -> String {
    let request = task_runner::request(task, task.image.clone(), Vec::new(), None);
    request.spec.cache_key().expect("a valid task").to_string()
}

#[test]
fn prints_lowered_documents() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let (root, cache) = (folder.path(), folder.path().join("cache"));
    fixtures(root);
    let (code, printed, errors) = uwe(root, &cache, &["lower", "pipeline.cwl"]);
    assert_eq!((code, errors.as_str()), (0, ""), "{printed}");
    let document: Json = serde_json::from_str(&printed).expect("prints JSON");
    assert_eq!(document["wir_version"], "0.1");
    assert_eq!(
        document["graphs"][0]["ops"][1]["kind"]["task"]["stdout"],
        "out.txt"
    );
    let (code, _, errors) = uwe(root, &cache, &["lower", "update.cwl"]);
    assert_eq!(code, 0, "{errors}");
    let kept = "uwe: op update stays a native CWL op: the requirement InplaceUpdateRequirement\n";
    assert_eq!(errors, kept);
}

/// The jobs of the fixtures whose tools all lower.
const RUNS: [(&str, &str); 3] = [
    ("echo.cwl", "message.yml"),
    ("count.cwl", "count.json"),
    ("pipeline.cwl", "message.yml"),
];

#[test]
fn matches_native_steps() {
    for (document, job) in RUNS {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let (root, cache) = (folder.path(), folder.path().join("cache"));
        fixtures(root);
        let native = ["run", "-executor", "simulator", document, "-inputs", job];
        let (code, _, errors) = uwe(root, &cache, &native);
        assert_eq!(code, 0, "{document}: {errors}");
        let (language, alone) = latest(&cache);
        assert_eq!(language, "cwl");
        let lowered: Vec<&str> = native.iter().copied().chain(["-lowered"]).collect();
        let (code, _, errors) = uwe(root, &cache, &lowered);
        assert_eq!(code, 0, "{document}: {errors}");
        let (language, through) = latest(&cache);
        assert_eq!(language, "wir", "{document} runs as wir");
        assert_eq!(alone.len(), through.len(), "{document}: the same tasks");
        for ((one, _, _), (other, _, _)) in alone.iter().zip(&through) {
            assert_eq!(one.step, other.step, "{document}: the same steps");
            assert_eq!(one.image, other.image, "{document}: the same image");
            // A compiled tool builds the task of the CWL driver, see docs/wir.md.
            assert_eq!(one.command, other.command, "{document}: {}", one.step);
            let paths = |task: &Task| -> Vec<String> {
                task.inputs.iter().map(|input| input.path.clone()).collect()
            };
            assert_eq!(paths(one), paths(other), "{document}: {}", one.step);
            assert_eq!(key(one), key(other), "{document}: {}", one.step);
            assert_eq!(one.workdir.as_deref(), Some("/var/spool/cwl"));
            assert_eq!(one.workdir, other.workdir, "{document}: {}", other.step);
            let variables: Vec<&String> = one.environment.keys().collect();
            assert_eq!(variables, ["HOME", "TMPDIR"]);
            assert_eq!(
                one.environment, other.environment,
                "{document}: {}",
                other.step
            );
        }
        let resumed: Vec<&str> = lowered.iter().copied().chain(["-resume"]).collect();
        let (code, _, errors) = uwe(root, &cache, &resumed);
        assert_eq!(code, 0, "{document}: {errors}");
        let (_, again) = latest(&cache);
        for ((one, print, _), (other, again, reused)) in through.iter().zip(&again) {
            assert_eq!(
                (key(one), print),
                (key(other), again),
                "{document}: {}",
                one.step
            );
            assert!(reused.is_some(), "{document}: {} is resumed", one.step);
        }
    }
}

#[test]
fn matches_scatter_steps() {
    for (document, job, count) in [
        ("scatter.cwl", "messages.json", 3),
        ("cross.cwl", "lists.json", 4),
    ] {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let (root, cache) = (folder.path(), folder.path().join("cache"));
        fixtures(root);
        let native = ["run", "-executor", "simulator", document, "-inputs", job];
        let (code, _, errors) = uwe(root, &cache, &native);
        assert_eq!(code, 0, "{document}: {errors}");
        let (_, alone) = latest(&cache);
        let lowered: Vec<&str> = native.iter().copied().chain(["-lowered"]).collect();
        let (code, _, errors) = uwe(root, &cache, &lowered);
        assert_eq!(code, 0, "{document}: {errors}");
        let (_, through) = latest(&cache);
        assert_eq!((alone.len(), through.len()), (count, count), "{document}");
        let keys = |tasks: &[(Task, String, Option<String>)]| {
            let mut keys: Vec<String> = tasks.iter().map(|(task, _, _)| key(task)).collect();
            keys.sort();
            keys
        };
        assert_eq!(keys(&alone), keys(&through), "{document}: the same tasks");
        let mut names: Vec<(&str, &str)> = (alone.iter().zip(&through))
            .map(|((one, _, _), (other, _, _))| (one.step.as_str(), other.step.as_str()))
            .collect();
        names.dedup();
        // The task of a round keeps the path of the map and of the op of its body.
        assert_eq!(names, [("say", "say/say")], "{document}");
        let mut rounds: Vec<&str> = through
            .iter()
            .map(|(task, _, _)| task.name.as_str())
            .collect();
        rounds.sort();
        assert_eq!(rounds[0], "say/0/say", "{document}");
    }
}

#[test]
fn matches_when_steps() {
    for (job, steps) in [("open.json", vec!["say/say"]), ("shut.json", vec![])] {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let (root, cache) = (folder.path(), folder.path().join("cache"));
        fixtures(root);
        let native = ["run", "-executor", "simulator", "gate.cwl", "-inputs", job];
        let (code, _, errors) = uwe(root, &cache, &native);
        assert_eq!(code, 0, "{job}: {errors}");
        let (_, alone) = latest(&cache);
        let lowered: Vec<&str> = native.iter().copied().chain(["-lowered"]).collect();
        let (code, _, errors) = uwe(root, &cache, &lowered);
        assert_eq!(code, 0, "{job}: {errors}");
        let (_, through) = latest(&cache);
        let alone: Vec<&str> = alone
            .iter()
            .map(|(task, _, _)| task.step.as_str())
            .collect();
        let through: Vec<&str> = through
            .iter()
            .map(|(task, _, _)| task.step.as_str())
            .collect();
        // The task of a branch has the path of the branch and of the op of its body.
        let native: Vec<&str> = steps.iter().map(|_| "say").collect();
        assert_eq!((alone, through), (native, steps), "{job}");
    }
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn matches_native_outputs() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let (root, cache) = (folder.path(), folder.path().join("cache"));
    fixtures(root);
    let words = [
        "run",
        "pipeline.cwl",
        "-inputs",
        "message.yml",
        "-no-network",
    ];
    let native: Vec<&str> = words
        .iter()
        .copied()
        .chain(["-output-dir", "native"])
        .collect();
    let (code, _, errors) = uwe(root, &cache, &native);
    assert_eq!(code, 0, "{errors}");
    let lowered: Vec<&str> = (words.iter().copied())
        .chain(["-lowered", "-output-dir", "lowered"])
        .collect();
    let (code, _, errors) = uwe(root, &cache, &lowered);
    assert_eq!(code, 0, "{errors}");
    let read = |path: &str| std::fs::read_to_string(root.join(path)).expect("reads an output");
    assert_eq!(read("native/out.txt"), "hello");
    assert_eq!(read("lowered/out.txt"), read("native/out.txt"));
    // wc prints the path of its input, which both stage in the same place.
    assert!(read("native/count.txt").contains("/var/lib/cwl/inputs/0/out.txt"));
    assert_eq!(read("lowered/count.txt"), read("native/count.txt"));
}
