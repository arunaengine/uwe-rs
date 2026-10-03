//! Reproducibility in whole runs: what a run records to pin it, what changed since, the order
//! check, and, with Docker, the reproducibility check of cached tasks and a repeat of a run
//! whose workflow and remote file changed since. The Docker tests need the
//! `debian:stable-slim` image; run them with
//! `cargo test -p iunctus --test reproducibility -- --ignored --test-threads 1`. A repeat asks
//! the registries whether the tags of its images moved; inside
//! `unshare --map-current-user -n --keep-caps` (with `ip link set lo up`) it cannot, and says so.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use run_database::{Database, SourceKind};

mod common;

use common::WebServer;

/// Reads a sample sheet from the web while it plans, and runs one task per row.
const SHEET_WORKFLOW: &str = r#"
params.sheet = null

process SAY {
    input:
    val id
    output:
    path "${id}.txt"
    script:
    """
    echo ${id} > ${id}.txt
    """
}

workflow {
    channel.of(file(params.sheet)).splitCsv(header: true).map { row -> row.id } | SAY
}
"#;

/// Collects files in the order their tasks finish, unless `SORTED` sorts them.
const COLLECT_WORKFLOW: &str = r#"
process MAKE {
    input:
    val x
    output:
    path "${x}.txt"
    script:
    """
    echo ${x} > ${x}.txt
    """
}

process JOIN {
    input:
    path files
    output:
    path 'all.txt'
    script:
    """
    cat ${files} > all.txt
    """
}

workflow {
    MAKE(channel.of(1, 2, 3))
    JOIN(MAKE.out.collect()SORTED)
}
"#;

/// Counts the bytes of a file on the web, and writes the time, which differs in every run.
const REAL_WORKFLOW: &str = r#"
params.data = null

process COUNT {
    container 'debian:stable-slim'
    input:
    path data
    output:
    path 'count.txt'
    script:
    """
    wc -c < ${data} > count.txt
    """
}

process STAMP {
    container 'debian:stable-slim'
    output:
    path 'stamp.txt'
    script:
    """
    date +%s%N > stamp.txt
    """
}

workflow {
    COUNT(channel.of(file(params.data)))
    STAMP()
}
"#;

/// Lists the network interfaces that the task sees.
const NETWORK_WORKFLOW: &str = r#"
process NETS {
    container 'debian:stable-slim'
    output:
    path 'nets.txt'
    script:
    """
    ls /sys/class/net > nets.txt
    """
}

workflow {
    NETS()
}
"#;

/// Runs the binary in `directory` with the cache folder `cache`, and gives its exit code and
/// what it printed.
fn iunctus(directory: &Path, cache: &Path, arguments: &[&str]) -> (i32, String) {
    let output = common::command()
        .args(arguments)
        .current_dir(directory)
        .env("XDG_CACHE_HOME", cache)
        .output()
        .expect("runs iunctus");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    (
        output.status.code().unwrap_or(-1),
        format!("{stdout}{stderr}"),
    )
}

fn database(cache: &Path) -> Database {
    Database::open(&cache.join("iunctus/runs.sqlite")).expect("opens the run database")
}

/// The id of the latest run in the database.
fn latest(cache: &Path) -> String {
    database(cache).runs(1).expect("lists the runs")[0]
        .id
        .clone()
}

#[test]
fn reports_pinned_changes() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let (folder, cache) = (root.path().join("pipeline"), root.path().join("cache"));
    std::fs::create_dir_all(&folder).expect("makes the folder");
    std::fs::write(folder.join("main.nf"), SHEET_WORKFLOW).expect("writes");
    std::fs::write(folder.join("extra.config"), "params.unused = 1\n").expect("writes");
    let server = WebServer::start(&[("/sheet.csv", "id\na\nb\n")]);
    let sheet = format!("{}/sheet.csv", server.address);
    let (code, output) = iunctus(
        &folder,
        &cache,
        &["run", "main.nf", "--sheet", &sheet, "-c", "extra.config"],
    );
    assert_eq!(code, 0, "{output}");
    let run = latest(&cache);
    let database = database(&cache);
    let recorded = database.run(&run).expect("reads").expect("the run");
    assert!(recorded.program.expect("a program").starts_with("iunctus "));
    let canonical = folder.canonicalize().expect("a folder");
    let launch = Path::new(recorded.launch.as_deref().expect("a launch folder"));
    assert_eq!(launch.canonicalize().expect("the folder"), canonical);
    let sources = database.sources(&run).expect("reads the sources");
    let kinds: Vec<(SourceKind, String)> = (sources.iter())
        .map(|source| {
            let name = Path::new(&source.path).file_name().expect("a name");
            (source.kind, name.to_string_lossy().into_owned())
        })
        .collect();
    assert!(
        kinds.contains(&(SourceKind::Workflow, "main.nf".into())),
        "{kinds:?}"
    );
    assert!(
        kinds.contains(&(SourceKind::Named, "extra.config".into())),
        "{kinds:?}"
    );
    let remote = database.remote_files(&run).expect("reads the remote files");
    assert_eq!(remote.len(), 1, "{remote:?}");
    assert_eq!(remote[0].address, sheet);
    assert_eq!(remote[0].size, 7);
    assert!(remote[0].version.is_some(), "the ETag of the server");
    let (code, output) = iunctus(&folder, &cache, &["changes", &run]);
    assert_eq!(code, 0, "{output}");
    assert!(
        output.contains("Nothing changed since the run."),
        "{output}"
    );
    // The workflow and the sample sheet change.
    std::fs::write(
        folder.join("main.nf"),
        format!("{SHEET_WORKFLOW}// changed\n"),
    )
    .expect("writes");
    server.set("/sheet.csv", "id\na\nc\n");
    let (code, output) = iunctus(&folder, &cache, &["changes", &run]);
    assert_eq!(code, 0, "{output}");
    assert!(
        output.contains("The files of the workflow: 1 of"),
        "{output}"
    );
    assert!(output.contains("changed  main.nf"), "{output}");
    assert!(
        output.contains(&format!("changed  {sheet}: its version is")),
        "{output}"
    );
    // A simulated run keeps no copy of its workflow, so its repeat cannot restore it.
    let (code, output) = iunctus(&folder, &cache, &["repeat", &run]);
    assert_eq!(code, 1, "{output}");
    assert!(output.contains("keeps no copy"), "{output}");
}

#[test]
fn identifies_order_dependence() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let cache = root.path().join("cache");
    let script = COLLECT_WORKFLOW.replace("SORTED", "");
    std::fs::write(root.path().join("main.nf"), script).expect("writes");
    let (code, output) = iunctus(root.path(), &cache, &["check-order", "main.nf"]);
    assert_eq!(code, 1, "{output}");
    assert!(
        output.contains("1 step depends on the order in which tasks finished"),
        "{output}"
    );
    assert!(
        output.contains("  JOIN: its tasks read the same content"),
        "{output}"
    );
    // Sorted by name, the files come in the same order in both runs.
    let sorted = COLLECT_WORKFLOW.replace("SORTED", ".map { files -> files.sort { it.name } }");
    std::fs::write(root.path().join("main.nf"), sorted).expect("writes");
    let (code, output) = iunctus(root.path(), &cache, &["check-order", "main.nf"]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("No step depends on the order"), "{output}");
}

/// The text of the output `path` of the task of `step` in a run.
fn output_text(cache: &Path, run: &str, step: &str) -> String {
    let database = database(cache);
    let tasks = database.tasks(run).expect("reads the tasks");
    let task = (tasks.iter())
        .find(|task| task.step.ends_with(step))
        .expect("the task");
    let detail = database
        .task(run, task.id)
        .expect("reads")
        .expect("the task");
    std::fs::read_to_string(&detail.outputs[0].location).expect("reads the output")
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn repeats_pinned_workflow() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let (folder, cache) = (root.path().join("pipeline"), root.path().join("cache"));
    std::fs::create_dir_all(&folder).expect("makes the folder");
    std::fs::write(folder.join("main.nf"), REAL_WORKFLOW).expect("writes");
    let server = WebServer::start(&[("/data.txt", "twelve bytes")]);
    let data = format!("{}/data.txt", server.address);
    let run = |extra: &[&str]| {
        let mut arguments = vec!["run", "main.nf", "-executor", "local", "--data", &data];
        arguments.extend(extra);
        let (code, output) = iunctus(&folder, &cache, &arguments);
        assert_eq!(code, 0, "{output}");
        (latest(&cache), output)
    };
    let (first, _) = run(&[]);
    assert_eq!(output_text(&cache, &first, "COUNT").trim(), "12");
    let images = database(&cache).images(&first).expect("reads the images");
    assert!(images.iter().all(|image| image.pinned()), "{images:?}");
    // Every cached task runs again: COUNT gives the same bytes, STAMP does not.
    let (second, output) = run(&["-check-reproducibility", "100%"]);
    assert!(
        output.contains("[checked] COUNT") && output.contains("gave the same bytes"),
        "{output}"
    );
    assert!(output.contains("gave other bytes"), "{output}");
    let checks = database(&cache)
        .checks(&second, None)
        .expect("reads the checks");
    let mut verdicts: Vec<&str> = checks.iter().map(|check| check.verdict.as_str()).collect();
    verdicts.sort();
    assert_eq!(verdicts, ["differs", "same"]);
    // STAMP shares no results any more; COUNT still comes from the cache.
    let (_, output) = run(&[]);
    assert!(output.contains("[cached] COUNT"), "{output}");
    assert!(output.contains("[done] STAMP"), "{output}");
    // The workflow and the file on the web change; a repeat of the first run has neither
    // change: it counts the bytes the first run read.
    std::fs::write(
        folder.join("main.nf"),
        REAL_WORKFLOW.replace("wc -c", "wc -l"),
    )
    .expect("writes");
    server.set("/data.txt", "now it has more bytes");
    let (code, output) = iunctus(&folder, &cache, &["repeat", &first]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("changed  main.nf"), "{output}");
    assert!(output.contains(&format!("changed  {data}")), "{output}");
    let repeat = latest(&cache);
    assert_eq!(output_text(&cache, &repeat, "COUNT").trim(), "12");
    let recorded = database(&cache)
        .run(&repeat)
        .expect("reads")
        .expect("the run");
    assert_eq!(recorded.repeats.as_deref(), Some(first.as_str()));
    assert!(
        output.contains("STAMP"),
        "the repeat names the step that differs: {output}"
    );
    // The crate of the first run holds its workflow as it ran, not as it is now.
    let (code, output) = iunctus(&folder, &cache, &["export", &first, "-output", "crate"]);
    assert_eq!(code, 0, "{output}");
    let main = std::fs::read_to_string(folder.join("crate/main.nf")).expect("reads");
    assert_eq!(main, REAL_WORKFLOW);
    let kept = std::fs::read_to_string(folder.join("crate/workflow/main.nf")).expect("reads");
    assert_eq!(kept, REAL_WORKFLOW);
}

#[test]
fn rejects_unsupported_isolation() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let cache = root.path().join("cache");
    std::fs::write(root.path().join("main.nf"), NETWORK_WORKFLOW).expect("writes");
    let singularity = &["-executor", "slurm", "-slurm-container", "singularity"][..];
    for executor in [
        &["-executor", "tes"][..],
        &["-executor", "kubernetes"][..],
        singularity,
    ] {
        let mut arguments = vec!["run", "main.nf", "-no-network"];
        arguments.extend(executor);
        let (code, output) = iunctus(root.path(), &cache, &arguments);
        assert_eq!(code, 2, "{output}");
        assert!(
            output.contains("cannot run without network there yet"),
            "{output}"
        );
    }
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn enforces_network_policy() {
    let root = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(root.path().join("main.nf"), NETWORK_WORKFLOW).expect("writes");
    let nets = |cache: &str, extra: &[&str], config: Option<&str>| {
        let cache = root.path().join(cache);
        let mut arguments = vec!["run", "main.nf", "-executor", "local"];
        arguments.extend(extra);
        let mut command = common::command();
        if let Some(text) = config {
            let file = root.path().join("network.toml");
            std::fs::write(&file, text).expect("writes the config file");
            arguments.extend(["-iunctus-config", "network.toml"]);
        }
        let output = command
            .args(&arguments)
            .current_dir(root.path())
            .env("XDG_CACHE_HOME", &cache)
            .output()
            .expect("runs iunctus");
        assert!(output.status.success(), "{output:?}");
        output_text(&cache, &latest(&cache), "NETS")
    };
    assert!(nets("open", &[], None).contains("eth0"));
    assert_eq!(nets("closed", &["-no-network"], None).trim(), "lo");
    let allowed = "[network]\ndefault = \"none\"\nallowed_steps = [\"NE*\"]\n";
    assert!(nets("allowed", &[], Some(allowed)).contains("eth0"));
}
