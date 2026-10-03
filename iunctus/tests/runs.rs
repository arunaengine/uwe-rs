//! Every `iunctus run` records itself in the run database: `iunctus runs` lists it, `-resume`
//! continues it without running its finished tasks again, and `iunctus export` writes its
//! RO-Crate. The runs use the simulator, so they need no container engine, except the ignored
//! test of resume with real containers: `cargo test -p iunctus --test runs -- --ignored`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

mod common;

const SCRIPT: &str = r#"
process UPPER {
    input:
    tuple val(id), path(text)
    output:
    tuple val(id), path("${id}.upper.txt")
    script:
    """
    tr a-z A-Z < ${text} > ${id}.upper.txt
    """
}

process COUNT {
    input:
    tuple val(id), path(text)
    output:
    path("${id}.count.txt")
    script:
    """
    wc -l < ${text} > ${id}.count.txt
    """
}

workflow {
    UPPER(channel.of(['a', file('a.txt')], ['b', file('b.txt')]))
    COUNT(UPPER.out)
}
"#;

/// Runs the binary in `directory`; the result has the exit code and the standard output and
/// error.
fn iunctus(directory: &Path, arguments: &[&str]) -> (i32, String) {
    let output = common::command()
        .args(arguments)
        .current_dir(directory)
        .env("XDG_CACHE_HOME", directory.join("cache"))
        .output()
        .expect("runs iunctus");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    (
        output.status.code().unwrap_or(-1),
        format!("{stdout}{stderr}"),
    )
}

/// The lines of a run that name tasks, sorted, like `[resumed] COUNT (1) ...`.
fn tasks(output: &str) -> Vec<String> {
    let mut lines: Vec<String> = output
        .lines()
        .filter(|line| line.starts_with('['))
        .map(|line| line.split(" (from run").next().unwrap_or(line).to_owned())
        .collect();
    lines.sort();
    lines
}

#[test]
fn preserves_run_history() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path();
    std::fs::write(path.join("main.nf"), SCRIPT).expect("writes the script");
    std::fs::write(path.join("a.txt"), "one\ntwo\n").expect("writes an input");
    std::fs::write(path.join("b.txt"), "three\n").expect("writes an input");

    let (code, output) = iunctus(path, &["run", "main.nf", "-resume"]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("no earlier run"), "{output}");
    let (code, output) = iunctus(path, &["runs"]);
    assert_eq!(code, 0, "{output}");
    let lines: Vec<&str> = output.lines().collect();
    assert_eq!(lines.len(), 2, "{output}");
    let first = lines[1]
        .split_whitespace()
        .next()
        .expect("a run id")
        .to_owned();
    assert!(
        lines[1].contains("succeeded") && lines[1].contains("simulated"),
        "{output}"
    );

    // Nothing changed, so every task comes from the first run.
    let (code, output) = iunctus(path, &["run", "main.nf", "-resume", &first]);
    assert_eq!(code, 0, "{output}");
    let expected = [
        "[resumed] COUNT (1)",
        "[resumed] COUNT (2)",
        "[resumed] UPPER (1)",
        "[resumed] UPPER (2)",
    ];
    assert_eq!(tasks(&output), expected, "{output}");

    // A changed input runs its task and the task after it again.
    std::fs::write(path.join("b.txt"), "four\nfive\n").expect("changes an input");
    let (code, output) = iunctus(path, &["run", "main.nf", "-resume"]);
    assert_eq!(code, 0, "{output}");
    let expected = [
        "[resumed] COUNT (1)",
        "[resumed] UPPER (1)",
        "[simulated] COUNT (2)",
        "[simulated] UPPER (2)",
    ];
    assert_eq!(tasks(&output), expected, "{output}");

    let (code, output) = iunctus(path, &["runs"]);
    assert_eq!(code, 0, "{output}");
    let last = output
        .lines()
        .nth(1)
        .and_then(|line| line.split_whitespace().next());
    let last = last.expect("a run id").to_owned();
    // Why the two tasks ran again: the changed input, and the task that ran before them.
    let answers: Vec<String> = (1..=4)
        .map(|task| iunctus(path, &["why", &last, &task.to_string()]).1)
        .collect();
    let text = answers.join("");
    assert!(
        text.contains("the input `b.txt` changed: 6 bytes before, 10 bytes now"),
        "{text}"
    );
    assert!(text.contains("which ran again"), "{text}");
    let (code, output) = iunctus(path, &["export", &last, "-output", "crate"]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("4 tasks"), "{output}");
    let metadata = std::fs::read_to_string(path.join("crate/ro-crate-metadata.json"))
        .expect("reads the metadata");
    let metadata: serde_json::Value = serde_json::from_str(&metadata).expect("JSON");
    let graph = metadata["@graph"].as_array().expect("a graph");
    let actions = graph
        .iter()
        .filter(|entity| entity["@type"] == "CreateAction")
        .count();
    // Four tasks and the workflow run.
    assert_eq!(actions, 5);
    assert!(path.join("crate/main.nf").is_file());

    let (code, output) = iunctus(path, &["origin", "no-such-file"]);
    assert_eq!(code, 0, "{output}");
    assert!(output.starts_with("no run made"), "{output}");

    let (code, output) = iunctus(path, &["export", "no-such-run"]);
    assert_eq!(code, 1, "{output}");
    assert!(output.contains("no run `no-such-run`"), "{output}");
}

/// Whether a report refers to nothing outside its file: every address in it is the name space
/// of SVG, which no browser loads.
fn self_contained(html: &str) -> bool {
    let addresses = html.matches("http://").count() + html.matches("https://").count();
    addresses == html.matches("http://www.w3.org/").count()
        && !html.contains("<link")
        && !html.contains("<script src")
}

#[test]
fn report_location_configurable() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path();
    std::fs::write(path.join("main.nf"), SCRIPT).expect("writes the script");
    std::fs::write(path.join("a.txt"), "one\n").expect("writes an input");
    std::fs::write(path.join("b.txt"), "two\n").expect("writes an input");
    let (code, output) = iunctus(path, &["run", "main.nf", "-report", "report.html"]);
    assert_eq!(code, 0, "{output}");
    let report = path.join("report.html");
    assert!(
        output.contains(&format!("{}", report.display())),
        "{output}"
    );
    let html = std::fs::read_to_string(&report).expect("reads the report");
    assert!(self_contained(&html));
    assert!(
        html.contains("UPPER (2)") && html.contains("COUNT"),
        "{html}"
    );

    let config = path.join("iunctus.toml");
    std::fs::write(&config, "[report]\nfile = \"reports/last.html\"\n").expect("writes");
    let config = config.to_string_lossy();
    let (code, output) = iunctus(path, &["run", "main.nf", "-iunctus-config", &config]);
    assert_eq!(code, 0, "{output}");
    assert!(path.join("reports/last.html").is_file(), "{output}");

    let (_, runs) = iunctus(path, &["runs"]);
    let id = runs
        .lines()
        .nth(1)
        .and_then(|line| line.split_whitespace().next());
    let id = id.expect("a run");
    let (code, output) = iunctus(path, &["report", id, "-output", "again.html"]);
    assert_eq!(code, 0, "{output}");
    assert!(output.starts_with(&format!("wrote the report of run {id} to again.html (")));
    let html = std::fs::read_to_string(path.join("again.html")).expect("reads the report");
    assert!(self_contained(&html) && html.contains(id));
    let (code, output) = iunctus(path, &["report", "no-such-run"]);
    assert_eq!(code, 1, "{output}");
    assert!(output.contains("no run `no-such-run`"), "{output}");
}

#[test]
fn rejects_simulated_resume() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path();
    std::fs::write(path.join("main.nf"), SCRIPT).expect("writes the script");
    std::fs::write(path.join("a.txt"), "one\n").expect("writes an input");
    std::fs::write(path.join("b.txt"), "two\n").expect("writes an input");
    let (code, output) = iunctus(path, &["run", "main.nf"]);
    assert_eq!(code, 0, "{output}");
    let (_, runs) = iunctus(path, &["runs"]);
    let id = runs
        .lines()
        .nth(1)
        .and_then(|line| line.split_whitespace().next());
    let arguments = [
        "run",
        "main.nf",
        "-executor",
        "local",
        "-resume",
        id.expect("a run"),
    ];
    let (code, output) = iunctus(path, &arguments);
    assert_eq!(code, 1, "{output}");
    assert!(output.contains("was simulated"), "{output}");
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn resumes_container_runs() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path();
    let script = SCRIPT.replace(
        "    input:\n",
        "    container 'debian:stable-slim'\n    input:\n",
    );
    std::fs::write(path.join("main.nf"), script).expect("writes the script");
    std::fs::write(path.join("a.txt"), "one\ntwo\n").expect("writes an input");
    std::fs::write(path.join("b.txt"), "three\n").expect("writes an input");
    let arguments = ["run", "main.nf", "-executor", "local", "-resume"];
    let (code, output) = iunctus(path, &arguments);
    assert_eq!(code, 0, "{output}");
    std::fs::write(path.join("b.txt"), "four\nfive\n").expect("changes an input");
    let (code, output) = iunctus(path, &arguments);
    assert_eq!(code, 0, "{output}");
    // Real tasks run at once and Nextflow numbers them in the order they end, so only the
    // kinds of the lines are the same in every run.
    let kinds: Vec<String> = tasks(&output)
        .iter()
        .map(|line| line.split(' ').next().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        kinds,
        ["[done]", "[done]", "[resumed]", "[resumed]"],
        "{output}"
    );
}
