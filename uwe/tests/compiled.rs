//! Prints the wir documents that Snakefiles compile into with `uwe lower`, names the parts
//! that stay native Snakemake ops, and runs compiled Snakefiles through the wir runtime.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::Value as Json;

mod common;

/// Runs `uwe lower Snakefile` on a Snakefile in a folder of its own; its exit code, what it
/// printed on standard output, and on standard error.
fn lower(snakefile: &str) -> (i32, String, String) {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let root = folder.path();
    std::fs::write(root.join("Snakefile"), snakefile).expect("writes the Snakefile");
    let output = common::command()
        .args(["lower", "Snakefile"])
        .current_dir(root)
        .env("XDG_CACHE_HOME", root.join("cache"))
        .output()
        .expect("runs uwe");
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    let code = output.status.code().unwrap_or(-1);
    assert!(
        !root.join(".snakemake/locks").exists(),
        "planning takes no lock"
    );
    (code, text(&output.stdout), text(&output.stderr))
}

#[test]
fn prints_compiled_snakefiles() {
    let snakefile = "rule all:\n    input: \"b.txt\"\n\nrule a:\n    output: temp(\"a.txt\")\n    shell: \"echo a > {output}\"\n\nrule b:\n    input: \"a.txt\"\n    output: \"b.txt\"\n    shell: \"cp {input} {output}\"\n";
    let (code, printed, errors) = lower(snakefile);
    assert_eq!((code, errors.as_str()), (0, ""), "{printed}");
    let document: Json = serde_json::from_str(&printed).expect("prints JSON");
    assert_eq!(document["sources"][0]["location"], "Snakefile");
    let ops = document["graphs"][0]["ops"].as_array().expect("ops");
    let calls: Vec<&str> = (ops.iter())
        .filter_map(|op| op["kind"]["task"]["builder"]["snakemake_job"]["task"]["call"].as_str())
        .collect();
    assert_eq!(calls, ["a", "b", "all"]);
    // Two files between jobs, and the target files to the output of the graph.
    assert_eq!(
        document["graphs"][0]["edges"].as_array().map(Vec::len),
        Some(3)
    );
    assert_eq!(document["graphs"][0]["outputs"][0]["name"], "targets");
}

#[test]
fn names_native_parts() {
    let snakefile = "storage:\n    provider=\"xrootd\"\n\nrule all:\n    output: \"a.txt\"\n    shell: \"touch {output}\"\n";
    let (code, printed, errors) = lower(snakefile);
    assert_eq!(code, 0, "{errors}");
    let expected = "uwe: op snakemake stays a native Snakemake op: Snakefile:1: Not supported yet:";
    assert!(errors.starts_with(expected), "{errors}");
    let document: Json = serde_json::from_str(&printed).expect("prints JSON");
    let native = &document["graphs"][0]["ops"][0]["kind"]["native"];
    assert_eq!(native["language"], "snakemake");
}

/// A group of jobs in two images, which keeps a workflow one native op.
const IMAGES: &str = "rule all:\n    input: \"b.txt\"\n\nrule a:\n    input: \"in.txt\"\n    output: \"a.txt\"\n    group: \"g\"\n    container: \"docker://alpine\"\n    shell: \"cp {input} {output}\"\n\nrule b:\n    input: \"a.txt\"\n    output: \"b.txt\"\n    group: \"g\"\n    container: \"docker://debian\"\n    shell: \"cp {input} {output}\"\n";

const WORD: &str = "rule all:\n    input: \"b.txt\"\n\nrule a:\n    output: \"a.txt\"\n    shell: \"echo {config[word]} > {output}\"\n\nrule b:\n    input: \"a.txt\"\n    output: \"b.txt\"\n    shell: \"cp {input} {output}\"\n";

/// Runs `uwe graph <arguments> -format json` on `snakefile` in a folder of its own,
/// with `files` beside it; the folder and the graph it printed.
fn graph_lowered(
    snakefile: &str,
    files: &[(&str, &str)],
    arguments: &[&str],
) -> (tempfile::TempDir, Json) {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let root = folder.path();
    std::fs::write(root.join("Snakefile"), snakefile).expect("writes the Snakefile");
    for (name, text) in files {
        std::fs::write(root.join(name), text).expect("writes a file");
    }
    let output = common::command()
        .args(["graph", "Snakefile", "-format", "json"])
        .args(arguments)
        .current_dir(root)
        .env("XDG_CACHE_HOME", root.join("cache"))
        .output()
        .expect("runs uwe");
    let errors = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{errors}");
    let graph = serde_json::from_slice(&output.stdout).expect("prints JSON");
    (folder, graph)
}

/// `uwe graph` runs the jobs of a compiled Snakefile through the job adapter, with
/// the config values of the command line in the document.
#[test]
fn runs_compiled_snakefiles() {
    let (_folder, graph) = graph_lowered(WORD, &[], &["--config", "word=hello"]);
    let nodes = graph["nodes"].as_array().expect("nodes");
    let names: Vec<&str> = (nodes.iter())
        .filter_map(|node| node["task"]["name"].as_str())
        .collect();
    assert_eq!(names, ["a", "b", "all"]);
    assert!(
        nodes[0]["task"]["script"]
            .as_str()
            .is_some_and(|script| script.contains("echo hello"))
    );
}

/// A config file of `-inputs` is a config source of the document, not an input of its graph.
#[test]
fn takes_config_inputs() {
    let (_folder, graph) = graph_lowered(
        WORD,
        &[("config.yaml", "word: hello\n")],
        &["-inputs", "config.yaml"],
    );
    let nodes = graph["nodes"].as_array().expect("nodes");
    let script = nodes[0]["task"]["script"].as_str().unwrap_or_default();
    assert!(script.contains("echo hello"), "{script}");
}

/// A Snakefile that stays one native op runs in the launch folder, where its inputs are.
#[test]
fn starts_in_place() {
    let (folder, graph) = graph_lowered(IMAGES, &[("in.txt", "raw\n")], &[]);
    let nodes = graph["nodes"].as_array().expect("nodes");
    let copy = (nodes.iter())
        .find(|node| node["task"]["step"] == "a")
        .expect("the task of rule a");
    let source = copy["task"]["inputs"][0]["source"]
        .as_str()
        .unwrap_or_default();
    assert_eq!(std::path::Path::new(source), folder.path().join("in.txt"));
}

/// The config values of a compiled run stay in no file beside its compiled document.
#[test]
fn keeps_values_private() {
    let (folder, graph) = graph_lowered(WORD, &[], &["--config", "word=hush4711"]);
    let script = graph["nodes"][0]["task"]["script"]
        .as_str()
        .unwrap_or_default();
    assert!(script.contains("hush4711"), "{script}");
    let lowered = folder.path().join("cache/uwe/scratch/lowered");
    assert!(lowered.is_dir());
    let mut pending = vec![lowered];
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            pending.extend(
                std::fs::read_dir(&path)
                    .unwrap()
                    .flatten()
                    .map(|entry| entry.path()),
            );
        } else if let Ok(text) = std::fs::read(&path) {
            let text = String::from_utf8_lossy(&text);
            assert!(
                !text.contains("hush4711"),
                "{} keeps the value",
                path.display()
            );
        }
    }
}

const HELLO: &str = "version 1.1\n\ntask hello {\n  input {\n    String name\n  }\n  command <<<\n    echo ~{name}\n  >>>\n  output {\n    File out = stdout()\n  }\n}\n";

/// The files of the values of a compiled run are no source of the run: a run records the
/// files of its workflow and keeps them, longer than the values may stay.
#[test]
fn records_no_values() {
    let wdl = HELLO;
    let runs = [
        ("Snakefile", WORD, ["--config", "word=hush4711"]),
        ("hello.wdl", wdl, ["--name", "hush4711"]),
    ];
    for (name, text, values) in runs {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let (root, cache) = (folder.path(), folder.path().join("cache"));
        std::fs::write(root.join(name), text).expect("writes the workflow");
        let output = common::command()
            .args(["run", "-executor", "simulator", name])
            .args(values)
            .current_dir(root)
            .env("XDG_CACHE_HOME", &cache)
            .output()
            .expect("runs uwe");
        let errors = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{errors}");
        let database = run_database::Database::open(&cache.join("uwe/runs.sqlite"))
            .expect("opens the run database");
        let run = database.runs(1).expect("lists the runs")[0].id.clone();
        let sources = database.sources(&run).expect("reads the sources");
        assert!(!sources.is_empty());
        for source in sources {
            let file = std::path::Path::new(&source.path)
                .file_name()
                .unwrap_or_default();
            let file = file.to_string_lossy();
            let private = file.starts_with("uwe-config-") || file.starts_with("uwe-inputs-");
            assert!(!private, "{name}: the run records {}", source.path);
        }
    }
}

/// A compiled run records its own document as its workflow, not the documents and values of
/// other runs beside it in the cache.
#[test]
fn records_own_document() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let (root, cache) = (folder.path(), folder.path().join("cache"));
    std::fs::write(root.join("hello.wdl"), HELLO).expect("writes the WDL file");
    std::fs::write(root.join("Snakefile"), WORD).expect("writes the Snakefile");
    let run = |arguments: &[&str]| {
        let output = common::command()
            .args(["run", "-executor", "simulator"])
            .args(arguments)
            .current_dir(root)
            .env("XDG_CACHE_HOME", &cache)
            .output()
            .expect("runs uwe");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let database = run_database::Database::open(&cache.join("uwe/runs.sqlite"))
            .expect("opens the run database");
        let id = database.runs(1).expect("lists the runs")[0].id.clone();
        let sources = database.sources(&id).expect("reads the sources");
        let paths: Vec<String> = sources.into_iter().map(|source| source.path).collect();
        (id, paths)
    };
    let (_, first) = run(&["hello.wdl", "--name", "hush4711"]);
    let (second, paths) = run(&["Snakefile", "--config", "word=hello"]);
    assert_eq!(paths.len(), 1, "{paths:?}");
    assert!(
        paths[0].ends_with(".wir.json") && !first.contains(&paths[0]),
        "{paths:?}"
    );
    let output = common::command()
        .args(["export", &second, "-output", "crate"])
        .current_dir(root)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .expect("runs uwe");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata = std::fs::read_to_string(root.join("crate/ro-crate-metadata.json"))
        .expect("reads the crate");
    for path in &first {
        assert!(!metadata.contains(path.as_str()), "the crate names {path}");
    }
    assert!(
        !metadata.contains("hush4711"),
        "the crate holds a value of the first run"
    );
}

/// A Nextflow pipeline whose compile reads the time, as nf-core's `trace_report_suffix` does.
const TIMED: &str = "params.stamp = new Date().getTime()\n\nprocess HELLO {\n    input:\n    val stamp\n    output:\n    path 'out.txt'\n    script:\n    \"echo ${stamp} > out.txt\"\n}\n\nworkflow {\n    HELLO(Channel.of(params.stamp))\n}\n";

/// A resume of a compiled run runs the document of the run it continues, with the same values
/// kept apart, also when compiling again gives another document, as with the time.
#[test]
fn resumes_compiled_runs() {
    let runs: [(&str, &str, &[&str]); 3] = [
        ("Snakefile", WORD, &["--config", "word=hush4711"]),
        ("hello.wdl", HELLO, &["--name", "hush4711"]),
        ("main.nf", TIMED, &[]),
    ];
    for (name, text, values) in runs {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let (root, cache) = (folder.path(), folder.path().join("cache"));
        std::fs::write(root.join(name), text).expect("writes the workflow");
        let run = |resume: &[&str]| {
            let output = common::command()
                .args(["run", "-executor", "simulator", name])
                .args(values)
                .args(resume)
                .current_dir(root)
                .env("XDG_CACHE_HOME", &cache)
                .output()
                .expect("runs uwe");
            let errors = String::from_utf8_lossy(&output.stderr).into_owned();
            assert!(output.status.success(), "{name}: {errors}");
            let database = run_database::Database::open(&cache.join("uwe/runs.sqlite"))
                .expect("opens the run database");
            database.runs(1).expect("lists the runs").remove(0).id
        };
        let first = run(&[]);
        // A document that no run wrote for over a week goes when another compiles.
        let lowered = cache.join("uwe/scratch/lowered");
        let stale = lowered.join("stale.wir.json");
        std::fs::write(&stale, "{}").expect("writes a stale document");
        let weeks = std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 24 * 3600);
        let file = std::fs::File::options()
            .write(true)
            .open(&stale)
            .expect("opens it");
        file.set_modified(weeks).expect("ages it");
        let resumed = run(&["-resume", &first]);
        // Every task of the resumed run comes from the first run.
        let database = run_database::Database::open(&cache.join("uwe/runs.sqlite"))
            .expect("opens the run database");
        let tasks = database.tasks(&resumed).expect("reads the tasks");
        assert!(!tasks.is_empty(), "{name}: the resumed run has tasks");
        let fresh: Vec<i64> = (tasks.iter())
            .filter(|task| task.reused.is_none())
            .map(|task| task.id as i64)
            .collect();
        assert!(fresh.is_empty(), "{name}: tasks {fresh:?} ran again");
        assert!(!stale.exists(), "{name}: the stale document went");
        let documents = (std::fs::read_dir(&lowered)
            .expect("lists the documents")
            .flatten())
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".wir.json"))
        .count();
        // The resume runs the document of the first run and writes no other.
        assert_eq!(documents, 1, "{name}: the documents of both runs");
    }
}

/// A resume with other config values than the run it continues is refused.
#[test]
fn refuses_other_values() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let (root, cache) = (folder.path(), folder.path().join("cache"));
    std::fs::write(root.join("Snakefile"), WORD).expect("writes the Snakefile");
    let run = |value: &str, resume: &[&str]| {
        common::command()
            .args([
                "run",
                "-executor",
                "simulator",
                "Snakefile",
                "--config",
                value,
            ])
            .args(resume)
            .current_dir(root)
            .env("XDG_CACHE_HOME", &cache)
            .output()
            .expect("runs uwe")
    };
    assert!(run("word=one", &[]).status.success());
    let database = run_database::Database::open(&cache.join("uwe/runs.sqlite"))
        .expect("opens the run database");
    let first = database.runs(1).expect("lists the runs").remove(0).id;
    let output = run("word=two", &["-resume", &first]);
    let errors = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success() && errors.contains("ran with other values"),
        "{errors}"
    );
}
