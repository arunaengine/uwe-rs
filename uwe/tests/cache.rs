//! Checks cache sharing, retention and preview across runs and workflow languages.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

mod common;

/// A module as nf-core writes them: one process, vendored into each pipeline that uses it. It
/// prints the count to both streams, so that tasks on other data have logs of their own.
const MODULE: &str = r#"
process WORDCOUNT {
    container 'debian:stable-slim'
    input:
    tuple val(id), path(text)
    output:
    tuple val(id), path("${id}.count"), emit: count
    script:
    """
    wc -w < ${text} > ${id}.count
    """
}
"#;

/// A pipeline that calls the module from its entry workflow.
const FIRST: &str = r#"
include { WORDCOUNT } from './modules/wordcount/main'

workflow {
    WORDCOUNT(channel.of(['sample', file('data/sample.txt')]))
    WORDCOUNT.out.count.view { id, count -> "${id}: ${count.text.trim()} words" }
}
"#;

/// Another pipeline: the module sits in a subworkflow of another name, and a step of its own
/// reads its result.
const SECOND: &str = r#"
include { WORDCOUNT } from './modules/wordcount/main'

workflow PREPARE {
    take:
    samples
    main:
    WORDCOUNT(samples)
    emit:
    counts = WORDCOUNT.out.count
}

process REPORT {
    container 'debian:stable-slim'
    input:
    tuple val(id), path(count)
    output:
    path('report.txt')
    script:
    """
    echo "${id} has \$(cat ${count}) words" > report.txt
    """
}

workflow {
    PREPARE(channel.of(['sample', file('data/sample.txt')]))
    REPORT(PREPARE.out.counts).view { report -> report.text.trim() }
}
"#;

/// Runs the binary in `directory` with the cache folder `cache`, and gives its exit code and
/// what it printed.
fn uwe(directory: &Path, cache: &Path, arguments: &[&str]) -> (i32, String) {
    // Separate runs share stored results only for tasks that cannot reach the network.
    let offline = matches!(arguments.first(), Some(&"run" | &"graph"));
    let output = common::command()
        .args(arguments)
        .args(offline.then_some("-no-network"))
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

/// A pipeline in its own folder, with its own copy of the module and of the data.
fn pipeline(root: &Path, name: &str, script: &str) -> std::path::PathBuf {
    let folder = root.join(name);
    std::fs::create_dir_all(folder.join("modules/wordcount")).expect("makes the folders");
    std::fs::create_dir_all(folder.join("data")).expect("makes the folders");
    std::fs::write(folder.join("modules/wordcount/main.nf"), MODULE).expect("writes");
    std::fs::write(folder.join("data/sample.txt"), "one two three\n").expect("writes");
    std::fs::write(folder.join("main.nf"), script).expect("writes");
    folder
}

/// The text of each file below a folder.
fn delivered(folder: &Path) -> Vec<String> {
    let mut texts = Vec::new();
    for entry in std::fs::read_dir(folder)
        .expect("lists the folder")
        .flatten()
    {
        let path = entry.path();
        match path.is_dir() {
            true => texts.extend(delivered(&path)),
            false => texts.push(std::fs::read_to_string(path).expect("reads")),
        }
    }
    texts
}

/// The cache entries in the store of a cache folder.
fn entries(cache: &Path) -> usize {
    let folder = cache.join("uwe/store/ac/v1/network-denied");
    let entries = std::fs::read_dir(folder).map_or(Vec::new(), |entries| entries.collect());
    (entries.iter().flatten())
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".json"))
        .count()
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn pipelines_share_cache() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let cache = root.path().join("cache");
    let first = pipeline(root.path(), "first", FIRST);
    let second = pipeline(root.path(), "second", SECOND);
    let arguments = ["run", "main.nf", "-executor", "local"];
    let (code, output) = uwe(&first, &cache, &arguments);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("[done] WORDCOUNT (1)"), "{output}");
    assert!(output.contains("sample: 3 words"), "{output}");
    // The other pipeline asks for the same task: the same image, command, inputs by content
    // and environment. Its process has another name and sits elsewhere; the task is the same.
    let (code, output) = uwe(&second, &cache, &arguments);
    assert_eq!(code, 0, "{output}");
    assert!(
        output.contains("[cached] PREPARE:WORDCOUNT (1)"),
        "{output}"
    );
    assert!(output.contains("[done] REPORT (1)"), "{output}");
    assert!(output.contains("sample has 3 words"), "{output}");
    assert_eq!(entries(&cache), 2);
}

/// A CWL tool that counts the words of a file.
const TOOL: &str = r#"
cwlVersion: v1.2
class: CommandLineTool
requirements:
  DockerRequirement:
    dockerPull: debian:stable-slim
baseCommand: [bash, -c]
arguments:
  - wc -w < $(inputs.text.path) > count.txt
inputs:
  text: File
outputs:
  count:
    type: File
    outputBinding:
      glob: count.txt
"#;

/// An Argo workflow that counts the same words with its image's user and working directory.
const TEMPLATE: &str = r#"
apiVersion: argoproj.io/v1alpha1
kind: Workflow
metadata:
  generateName: count-
spec:
  entrypoint: count
  templates:
    - name: count
      inputs:
        artifacts:
          - name: text
            path: /var/lib/cwl/inputs/0/sample.txt
            raw:
              data: |
                one two three
      container:
        image: debian:stable-slim
        workingDir: /var/spool/cwl
        env:
          - name: HOME
            value: /var/spool/cwl
          - name: TMPDIR
            value: /tmp
        command: [bash, -c]
        args: ["wc -w < /var/lib/cwl/inputs/0/sample.txt > count.txt"]
      outputs:
        artifacts:
          - name: count
            path: /var/spool/cwl/count.txt
"#;

/// A Snakemake rule that does the same work as the CWL tool, as Snakemake describes it.
const RULE: &str = r#"
rule count:
    input: "sample.txt"
    output: "count.txt"
    container: "docker://debian:stable-slim"
    shell: "wc -w < {input} > {output}"
"#;

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn languages_share_cache() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let cache = root.path().join("cache");
    let folder = root.path();
    std::fs::write(folder.join("sample.txt"), "one two three\n").expect("writes");
    std::fs::write(folder.join("count.cwl"), TOOL).expect("writes");
    std::fs::write(
        folder.join("job.yml"),
        "text:\n  class: File\n  path: sample.txt\n",
    )
    .expect("writes");
    std::fs::write(folder.join("count.yaml"), TEMPLATE).expect("writes");
    std::fs::write(folder.join("Snakefile"), RULE).expect("writes");
    let cwl = [
        "run",
        "count.cwl",
        "-inputs",
        "job.yml",
        "-executor",
        "local",
    ];
    let output_dir = folder.join("argo").to_string_lossy().into_owned();
    let argo = [
        "run",
        "count.yaml",
        "-executor",
        "local",
        "-output-dir",
        &output_dir,
    ];
    let (code, output) = uwe(folder, &cache, &cwl);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("[done] count"), "{output}");
    // Argo keeps its image's user, so its task has a different execution identity.
    let (code, output) = uwe(folder, &cache, &argo);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("[done] count"), "{output}");
    let (code, output) = uwe(folder, &cache, &argo);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("[cached] count"), "{output}");
    // The artifact was delivered from Argo's stored result.
    assert_eq!(delivered(&folder.join("argo")), ["3\n"]);
    // The Snakemake rule does the same work, but Snakemake describes its jobs in its own way:
    // another command, no fixed working directory, other variables. So it runs.
    std::fs::remove_file(folder.join("count.txt")).expect("removes the CWL output");
    let (code, output) = uwe(folder, &cache, &["run", "Snakefile", "-executor", "local"]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("[done] count"), "{output}");
    // One result for each description of the task: the CWL tool, the Argo template and the rule.
    assert_eq!(entries(&cache), 3);
}

/// A pipeline whose one task takes a few seconds.
const SLOW: &str = r#"
process SLOW {
    container 'debian:stable-slim'
    output:
    path('done.txt')
    script:
    """
    sleep 4
    echo done > done.txt
    """
}

workflow {
    SLOW()
}
"#;

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn concurrent_runs_deduplicate() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let cache = root.path().join("cache");
    let folders: Vec<_> = ["one", "two"]
        .iter()
        .map(|name| {
            let folder = root.path().join(name);
            std::fs::create_dir_all(&folder).expect("makes the folder");
            std::fs::write(folder.join("main.nf"), SLOW).expect("writes");
            folder
        })
        .collect();
    // Both runs start at once, on the same store.
    let started: Vec<_> = folders
        .iter()
        .map(|folder| {
            common::command()
                .args(["run", "main.nf", "-executor", "local", "-no-network"])
                .current_dir(folder)
                .env("XDG_CACHE_HOME", &cache)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("runs uwe")
        })
        .collect();
    let mut lines = Vec::new();
    for child in started {
        let output = child.wait_with_output().expect("uwe ends");
        assert!(output.status.success(), "{output:?}");
        lines.push(common::without_display(&String::from_utf8_lossy(
            &output.stdout,
        )));
    }
    // One run ran the task; the other waited for it instead of running it too.
    lines.sort();
    assert_eq!(lines, ["[cached] SLOW\n", "[done] SLOW\n"], "{lines:?}");
    assert_eq!(entries(&cache), 1);
}

/// The ids of the runs in the run database of a cache folder, the latest first.
fn run_ids(cache: &Path) -> Vec<String> {
    let database = run_database::Database::open(&cache.join("uwe/runs.sqlite"));
    let runs = database.expect("opens").runs(50).expect("reads");
    runs.into_iter().map(|run| run.id).collect()
}

#[test]
fn clean_requires_rule() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let cache = root.path().join("cache");
    let first = pipeline(root.path(), "first", FIRST);
    let (code, output) = uwe(root.path(), &cache, &["clean"]);
    // A command without what it needs is a mistake of the command line.
    assert_eq!(code, 2, "{output}");
    assert!(output.contains("say what `uwe clean` keeps"), "{output}");
    for _ in 0..2 {
        let (code, output) = uwe(
            &first,
            &cache,
            &["run", "-executor", "simulator", "main.nf"],
        );
        assert_eq!(code, 0, "{output}");
    }
    let simulated = cache.join("uwe/scratch/simulator");
    assert_eq!(std::fs::read_dir(&simulated).expect("lists").count(), 2);
    let mut ids = run_ids(&cache);
    let (latest, earlier) = (ids.remove(0), ids.remove(0));
    // Folders of attempts that the ended run left, in the scratch folder of each executor; the
    // SIF files of SLURM jobs and folders of unknown runs stay.
    let scratch = cache.join("uwe/scratch");
    let attempts = ["", "tes/", "kubernetes/", "slurm/"]
        .map(|folder| scratch.join(format!("{folder}{earlier}-1-1")));
    let staying = [scratch.join("slurm/images"), scratch.join("slurm/1-2-3-4")];
    for folder in attempts.iter().chain(&staying) {
        std::fs::create_dir_all(folder.join("work")).expect("makes the folder");
    }
    let (code, output) = uwe(root.path(), &cache, &["clean", "-keep-run", &latest]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("It keeps what run"), "{output}");
    assert!(
        output.contains("scratch folders of runs that ended: 5 folders"),
        "{output}"
    );
    assert!(
        output.contains("Nothing was removed; add -remove"),
        "{output}"
    );
    assert_eq!(std::fs::read_dir(&simulated).expect("lists").count(), 2);
    let arguments = ["clean", "-keep-run", &latest, "-remove"];
    let (code, output) = uwe(root.path(), &cache, &arguments);
    assert_eq!(code, 0, "{output}");
    assert!(
        output.contains("Removed 0 results, 0 files and 5 scratch folders."),
        "{output}"
    );
    assert!(attempts.iter().all(|folder| !folder.exists()));
    assert!(staying.iter().all(|folder| folder.exists()));
    let left: Vec<String> = std::fs::read_dir(&simulated)
        .expect("lists")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(left, [latest]);
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn clean_respects_retention() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let cache = root.path().join("cache");
    let first = pipeline(root.path(), "first", FIRST);
    let second = pipeline(root.path(), "second", SECOND);
    let real = ["run", "main.nf", "-executor", "local"];
    assert_eq!(uwe(&first, &cache, &real).0, 0);
    // The second pipeline takes the count of the first from the cache.
    let (code, output) = uwe(&second, &cache, &real);
    assert_eq!(code, 0, "{output}");
    let kept = run_ids(&cache).remove(0);
    // The first pipeline counts other data.
    std::fs::write(first.join("data/sample.txt"), "one two three four\n").expect("writes");
    let (code, output) = uwe(&first, &cache, &real);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("sample: 4 words"), "{output}");
    let dropped = run_ids(&cache).remove(0);
    assert_eq!(entries(&cache), 3);
    // What the second run needs stays, also the count that it took from the first run; the
    // last run's result goes.
    let arguments = ["clean", "-keep-run", &kept];
    let (code, output) = uwe(root.path(), &cache, &arguments);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("holds 3 results"), "{output}");
    // The files that go: the result of the last run, and the contents that only runs that no
    // rule keeps read (the first script, the changed data) with the trees of the workflow.
    assert!(
        output.contains("It would remove 1 result and 7 files"),
        "{output}"
    );
    assert!(output.contains(&format!("run {dropped} (")), "{output}");
    assert_eq!(entries(&cache), 3);
    let arguments = ["clean", "-keep-run", &kept, "-remove"];
    let (code, output) = uwe(root.path(), &cache, &arguments);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("Removed 1 result, 7 files"), "{output}");
    assert_eq!(entries(&cache), 2);
    // The files of the workflow of the kept run stay, so that a repeat of it can restore them.
    for file in ["main.nf", "modules/wordcount/main.nf", "data/sample.txt"] {
        let bytes = std::fs::read(second.join(file)).expect("reads");
        let digest = task_model::Digest::sha256(&bytes);
        let stored = cache
            .join("uwe/store")
            .join(content_store::layout::content(&digest));
        assert!(stored.exists(), "{file} of the kept run is gone");
    }
    let (code, output) = uwe(&second, &cache, &real);
    assert_eq!(code, 0, "{output}");
    assert!(
        output.contains("[cached] PREPARE:WORDCOUNT (1)"),
        "{output}"
    );
    assert!(output.contains("[cached] REPORT (1)"), "{output}");
    let (code, output) = uwe(&first, &cache, &real);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("[done] WORDCOUNT (1)"), "{output}");
    // Without a rule that keeps anything, everything goes.
    let (code, output) = uwe(
        root.path(),
        &cache,
        &["clean", "-keep-days", "0", "-remove"],
    );
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("It keeps nothing"), "{output}");
    assert_eq!(entries(&cache), 0);
    let objects = |kind: &str| -> usize {
        let folder = cache.join("uwe/store").join(kind);
        std::fs::read_dir(folder).map_or(0, |entries| entries.count())
    };
    assert_eq!(
        (objects("runs"), objects("cas"), objects("trees")),
        (0, 0, 0)
    );
    let (code, output) = uwe(&second, &cache, &real);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("[done] PREPARE:WORDCOUNT (1)"), "{output}");
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn previews_cached_results() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let cache = root.path().join("cache");
    let first = pipeline(root.path(), "first", FIRST);
    let second = pipeline(root.path(), "second", SECOND);
    // Before any real run, everything is simulated.
    let (code, output) = uwe(
        &second,
        &cache,
        &["run", "-executor", "simulator", "main.nf"],
    );
    assert_eq!(code, 0, "{output}");
    assert!(
        output.contains("[simulated] PREPARE:WORDCOUNT (1)"),
        "{output}"
    );
    assert!(!output.contains("from the cache"), "{output}");
    assert_eq!(
        uwe(&first, &cache, &["run", "main.nf", "-executor", "local"]).0,
        0
    );
    // The count of the first pipeline answers the second, which a real run would take from
    // the cache; the report after it would run.
    let (code, output) = uwe(
        &second,
        &cache,
        &["run", "-executor", "simulator", "main.nf"],
    );
    assert_eq!(code, 0, "{output}");
    assert!(
        output.contains("[cached] PREPARE:WORDCOUNT (1)"),
        "{output}"
    );
    assert!(output.contains("[simulated] REPORT (1)"), "{output}");
    let note = "A real run would take 1 of 2 tasks from the cache; 1 would run.";
    assert!(output.contains(&format!("uwe: {note}")), "{output}");
    // The graph, as a summary and as a picture, says the same.
    let (code, summary) = uwe(&second, &cache, &["graph", "main.nf"]);
    assert_eq!(code, 0, "{summary}");
    assert!(summary.contains(&format!("{note}\n")), "{summary}");
    assert!(
        summary.contains("PREPARE:WORDCOUNT (1) (from the cache) <- []"),
        "{summary}"
    );
    let (code, steps) = uwe(&second, &cache, &["graph", "main.nf", "-view", "steps"]);
    assert_eq!(code, 0, "{steps}");
    assert!(
        steps.contains("PREPARE:WORDCOUNT: 1 task, 1 from the cache"),
        "{steps}"
    );
    let (code, picture) = uwe(&second, &cache, &["graph", "main.nf", "-format", "svg"]);
    assert_eq!(code, 0, "{picture}");
    assert!(picture.contains(note), "{picture}");
    assert!(picture.contains("reused"), "{picture}");
    // The run database knows it too, for the web page.
    let latest = run_ids(&cache).remove(0);
    let database = run_database::Database::open(&cache.join("uwe/runs.sqlite"));
    let graph = database.expect("opens").graph(&latest).expect("reads");
    let graph = graph.expect("the graph of the run");
    assert_eq!(graph.cache_note().as_deref(), Some(note));
    // Once the second pipeline ran for real, a preview finds all of it in the cache.
    assert_eq!(
        uwe(&second, &cache, &["run", "main.nf", "-executor", "local"]).0,
        0
    );
    let (code, output) = uwe(
        &second,
        &cache,
        &["run", "-executor", "simulator", "main.nf"],
    );
    assert_eq!(code, 0, "{output}");
    let all = "A real run would take all 2 tasks from the cache; none would run.";
    assert!(output.contains(all), "{output}");
    // Without the cache, a preview finds nothing in it, and a real run runs every task.
    let (code, output) = uwe(
        &second,
        &cache,
        &["run", "-executor", "simulator", "main.nf", "-no-cache"],
    );
    assert_eq!(code, 0, "{output}");
    assert!(!output.contains("[cached]"), "{output}");
    assert!(!output.contains("from the cache"), "{output}");
    let arguments = ["run", "main.nf", "-executor", "local", "-no-cache"];
    let (code, output) = uwe(&second, &cache, &arguments);
    assert_eq!(code, 0, "{output}");
    assert!(!output.contains("[cached]"), "{output}");
    assert_eq!(output.matches("[done]").count(), 2, "{output}");
}

/// Three short tasks and a chain of two long ones. Each task asks for more CPUs than any
/// machine has, so it gets all of them and the tasks run one after another. Each writes when
/// it started, in nanoseconds.
const CHAINS: &str = r#"
params.round = 1

process LONG {
    container 'debian:stable-slim'
    cpus 100000
    input:
    val(name)
    output:
    path('long.txt')
    script:
    """
    date +%s%N > long.txt
    sleep 2
    echo ${params.round} ${name} > /dev/null
    """
}

process AFTER {
    container 'debian:stable-slim'
    cpus 100000
    input:
    path(first)
    output:
    path('after.txt')
    script:
    """
    sleep 2
    cat ${first} > after.txt
    echo ${params.round} > /dev/null
    """
}

process SHORT {
    container 'debian:stable-slim'
    cpus 100000
    input:
    val(n)
    output:
    path("short.txt")
    script:
    """
    date +%s%N > short.txt
    echo ${params.round} ${n} > /dev/null
    """
}

workflow {
    SHORT(channel.of(1, 2, 3)).view { file -> "short ${file.text.trim()}" }
    AFTER(LONG(channel.of('long')))
    LONG.out.view { file -> "long ${file.text.trim()}" }
}
"#;

/// When the long task and each short task of a run of `CHAINS` started, in nanoseconds.
fn starts(directory: &Path, cache: &Path, round: &str) -> (u128, Vec<u128>) {
    let arguments = ["run", "main.nf", "-executor", "local", "--round", round];
    let (code, output) = uwe(directory, cache, &arguments);
    assert_eq!(code, 0, "{output}");
    let time = |line: &str| {
        line.split(' ')
            .nth(1)
            .and_then(|time| time.parse::<u128>().ok())
    };
    let long = (output.lines())
        .filter(|line| line.starts_with("long "))
        .find_map(time);
    let shorts = (output.lines())
        .filter(|line| line.starts_with("short "))
        .filter_map(time)
        .collect();
    (long.expect("the long task started"), shorts)
}

/// How many short tasks started before the long one.
fn shorts_before((long, shorts): &(u128, Vec<u128>)) -> usize {
    shorts.iter().filter(|short| *short < long).count()
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn longest_chain_first() {
    let root = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(root.path().join("main.nf"), CHAINS).expect("writes");
    // Which task a free machine takes first is a race. A pair of runs that loses it is
    // repeated in a new cache; a start that does not learn the times fails every pair.
    let mut pairs = Vec::new();
    for attempt in 0..3 {
        let cache = root.path().join(format!("cache-{attempt}"));
        // The first run tells how long each step takes; without times, the tasks start in
        // the order the workflow asked for them.
        let cold = starts(root.path(), &cache, "1");
        // The second run, whose tasks are new, starts the long chain before the short tasks
        // that wait: at most one short task that found the machine free starts before it.
        let warm = starts(root.path(), &cache, "2");
        assert_eq!(warm.1.len(), 3);
        let (before, after) = (shorts_before(&cold), shorts_before(&warm));
        if after <= 1 && after < before {
            return;
        }
        pairs.push((cold, warm));
    }
    panic!("the long task did not start before the short ones: {pairs:?}");
}

/// The config file of a machine whose store has its folder `front` in front of the shared store
/// in the folder `shared`, which a storage provider serves as `s3://shared/`, as a bucket
/// would.
fn layers(front: &Path, shared: &Path) -> std::path::PathBuf {
    let file = front.with_extension("toml");
    let text = format!(
        "[storage.shared]\ntype = \"folder\"\nprefixes = [\"s3://shared/\"]\npath = \"{}\"\n\n\
         [store]\nlocation = \"s3://shared/cache\"\nlocal = \"{}\"\n",
        shared.display(),
        front.display()
    );
    std::fs::write(&file, text).expect("writes the config file");
    file
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn layered_store_shares() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let shared = root.path().join("shared");
    let entries = |folder: &Path| {
        let folder = folder.join("ac/v1/network-denied");
        let entries = std::fs::read_dir(folder).map_or(Vec::new(), |entries| entries.collect());
        (entries.iter().flatten())
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".json"))
            .count()
    };
    // The first machine runs both tasks; their results go to its folder and to the shared
    // store.
    let first = pipeline(root.path(), "first", SECOND);
    let front = root.path().join("front-first");
    let config = layers(&front, &shared).to_string_lossy().into_owned();
    let arguments = [
        "run",
        "main.nf",
        "-executor",
        "local",
        "-uwe-config",
        &config,
    ];
    let (code, output) = uwe(&first, &root.path().join("cache-first"), &arguments);
    assert_eq!(code, 0, "{output}");
    assert_eq!(output.matches("[done]").count(), 2, "{output}");
    assert_eq!(entries(&front), 2);
    assert_eq!(entries(&shared.join("cache")), 2);
    // The second machine has an empty folder: it runs nothing, and keeps what it read.
    let second = pipeline(root.path(), "second", SECOND);
    let front = root.path().join("front-second");
    let config = layers(&front, &shared).to_string_lossy().into_owned();
    let arguments = [
        "run",
        "main.nf",
        "-executor",
        "local",
        "-uwe-config",
        &config,
    ];
    let (code, output) = uwe(&second, &root.path().join("cache-second"), &arguments);
    assert_eq!(code, 0, "{output}");
    assert_eq!(output.matches("[cached]").count(), 2, "{output}");
    assert!(output.contains("sample has 3 words"), "{output}");
    assert_eq!(entries(&front), 2);
}

/// The cache keys of the entries in the store of a cache folder.
fn keys(cache: &Path) -> std::collections::BTreeSet<String> {
    let folder = cache.join("uwe/store/ac/v1/network-denied");
    let entries = std::fs::read_dir(folder).map_or(Vec::new(), |entries| entries.collect());
    (entries.iter().flatten())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".json"))
        .collect()
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn uncached_process_reruns() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let cache = root.path().join("cache");
    let folder = pipeline(root.path(), "second", SECOND);
    let plain: &[&str] = &["run", "main.nf", "-executor", "local"];
    let never: &[&str] = &[plain, &["-no-cache-steps", "WORDCOUNT"]].concat();
    // The step runs and keeps no result; its file still reaches the step after it.
    for (arguments, report, stored) in [
        (never, "[done] REPORT (1)", 1),
        (never, "[cached] REPORT (1)", 1),
        (plain, "[cached] REPORT (1)", 2),
    ] {
        let (code, output) = uwe(&folder, &cache, arguments);
        assert_eq!(code, 0, "{output}");
        assert!(output.contains("[done] PREPARE:WORDCOUNT (1)"), "{output}");
        assert!(output.contains(report), "{output}");
        assert!(output.contains("sample has 3 words"), "{output}");
        assert_eq!(entries(&cache), stored, "{output}");
    }
    let (code, output) = uwe(&folder, &cache, plain);
    assert_eq!(code, 0, "{output}");
    assert!(
        output.contains("[cached] PREPARE:WORDCOUNT (1)"),
        "{output}"
    );
    // The rule changed no cache key: a cache that never saw it holds the same entries.
    let other = root.path().join("other");
    assert_eq!(uwe(&folder, &other, plain).0, 0);
    assert_eq!(keys(&cache), keys(&other));
}

/// A CWL workflow: the tool counts the words, and a second step writes a sentence with the count.
const COUNTED: &str = r#"
cwlVersion: v1.2
class: Workflow
inputs:
  text: File
outputs:
  report:
    type: File
    outputSource: report/report
steps:
  count:
    run: count.cwl
    in:
      text: text
    out: [count]
  report:
    run:
      class: CommandLineTool
      requirements:
        DockerRequirement:
          dockerPull: debian:stable-slim
      baseCommand: [bash, -c]
      arguments:
        - echo "sample has `cat $(inputs.count.path)` words" > report.txt
      inputs:
        count: File
      outputs:
        report:
          type: File
          outputBinding:
            glob: report.txt
    in:
      count: count/count
    out: [report]
"#;

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn uncached_step_reruns() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let cache = root.path().join("cache");
    let folder = root.path();
    std::fs::write(folder.join("sample.txt"), "one two three\n").expect("writes");
    std::fs::write(folder.join("count.cwl"), TOOL).expect("writes");
    std::fs::write(folder.join("main.cwl"), COUNTED).expect("writes");
    std::fs::write(
        folder.join("job.yml"),
        "text:\n  class: File\n  path: sample.txt\n",
    )
    .expect("writes");
    for (round, (never, report, stored)) in [
        (true, "[done] report", 1),
        (true, "[cached] report", 1),
        (false, "[cached] report", 2),
    ]
    .into_iter()
    .enumerate()
    {
        let results = folder.join(format!("results-{round}"));
        let mut arguments = vec![
            "run",
            "main.cwl",
            "-inputs",
            "job.yml",
            "-executor",
            "local",
        ];
        let results_text = results.to_string_lossy().into_owned();
        arguments.extend(["-output-dir", &results_text]);
        arguments.extend(
            never
                .then_some(["-no-cache-steps", "count"])
                .into_iter()
                .flatten(),
        );
        let (code, output) = uwe(folder, &cache, &arguments);
        assert_eq!(code, 0, "{output}");
        assert!(output.contains("[done] count"), "{output}");
        assert!(output.contains(report), "{output}");
        assert_eq!(delivered(&results), ["sample has 3 words\n"], "{output}");
        assert_eq!(entries(&cache), stored, "{output}");
    }
}
