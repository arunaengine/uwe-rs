//! Checks timing, comparison, replay and telemetry commands on recorded runs.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};

mod common;

const SCRIPT: &str = r#"
process UPPER {
    container 'debian:stable-slim'
    input:
    path text
    output:
    path "upper.txt"
    script:
    """
    tr a-z A-Z < ${text} > upper.txt
    """
}

process COUNT {
    container 'debian:stable-slim'
    input:
    path text
    output:
    path "count.txt"
    script:
    """
    wc -c < ${text} > count.txt
    echo "counted ${params.note}" >&2
    """
}

params.note = 'first'

workflow {
    COUNT(UPPER(channel.of(file('a.txt'))))
}
"#;

/// Runs the binary in `directory`; the result has the exit code and the standard output and
/// error.
fn iunctus(directory: &Path, arguments: &[&str]) -> (i32, String) {
    iunctus_with(directory, arguments, &[])
}

/// Runs the binary in `directory` with more environment variables.
fn iunctus_with(directory: &Path, arguments: &[&str], variables: &[(&str, &str)]) -> (i32, String) {
    let output = common::command()
        .args(arguments)
        .current_dir(directory)
        .env("XDG_CACHE_HOME", directory.join("cache"))
        .envs(variables.iter().copied())
        .output()
        .expect("runs iunctus");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    (
        output.status.code().unwrap_or(-1),
        format!("{stdout}{stderr}"),
    )
}

/// The runs of the database, the latest first.
fn runs(directory: &Path) -> Vec<String> {
    let (code, output) = iunctus(directory, &["runs"]);
    assert_eq!(code, 0, "{output}");
    let lines = output.lines().skip(1);
    lines
        .filter_map(|line| line.split_whitespace().next().map(str::to_owned))
        .collect()
}

fn workflow(directory: &Path) {
    std::fs::write(directory.join("main.nf"), SCRIPT).expect("writes the workflow");
    std::fs::write(directory.join("a.txt"), "some text\n").expect("writes the input");
}

#[test]
fn reports_run_insight() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path();
    workflow(path);
    let (code, output) = iunctus(path, &["run", "main.nf"]);
    assert_eq!(code, 0, "{output}");
    let (code, output) = iunctus(path, &["run", "main.nf", "--note", "second"]);
    assert_eq!(code, 0, "{output}");
    let [second, first] = runs(path).try_into().expect("two runs");

    let (code, output) = iunctus(path, &["time", &second]);
    assert_eq!(code, 0, "{output}");
    assert!(
        output.contains("the critical path, the chain of tasks"),
        "{output}"
    );
    assert!(output.contains("task 2 COUNT"), "{output}");
    assert!(
        output.contains("the times are those of the simulator"),
        "{output}"
    );
    let (code, output) = iunctus(path, &["time", &second, "-format", "json"]);
    assert_eq!(code, 0, "{output}");
    let time: serde_json::Value = serde_json::from_str(&output).expect("JSON");
    assert_eq!(time["critical_path"].as_array().map(Vec::len), Some(2));

    let (code, output) = iunctus(path, &["compare", &second]);
    assert_eq!(code, 0, "{output}");
    assert!(
        output.contains(&format!("comparing run {first}")),
        "{output}"
    );
    assert!(
        output.contains("the parameter `note` is not given in the first run and is `second`"),
        "{output}"
    );
    assert!(output.contains("1 the same, 1 changed"), "{output}");
    let (code, output) = iunctus(path, &["compare", &first, "no-run"]);
    assert_eq!(code, 1, "{output}");
    assert!(output.contains("has no run `no-run`"), "{output}");

    let (code, output) = iunctus(path, &["replay", &second, "2"]);
    assert_eq!(code, 1, "{output}");
    assert!(output.contains("was simulated"), "{output}");
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn replays_recorded_container() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path();
    workflow(path);
    // A real run sends its trace to the endpoint of the config file when it ends.
    let (endpoint, received) = receiver();
    let config = format!("[telemetry]\nendpoint = \"{endpoint}\"\n");
    std::fs::write(path.join("config.toml"), config).expect("writes the config");
    let arguments = [
        "run",
        "main.nf",
        "-executor",
        "local",
        "-iunctus-config",
        "config.toml",
    ];
    let (code, output) = iunctus_with(path, &arguments, &[("IUNCTUS_LOG", "info")]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("sent the trace to"), "{output}");
    let (_, body) = received.recv().expect("the trace");
    assert!(body.contains("\"iunctus.attempt.image\""), "{body}");
    let [run] = runs(path).try_into().expect("one run");
    // The file that COUNT read is gone from its place; the store still has it by its digest.
    let stored = path.join(format!(
        "cache/iunctus/store/runs/{run}/UPPER-1/1/work/upper.txt"
    ));
    std::fs::remove_file(&stored).expect("removes the stored output");

    let (code, output) = iunctus(path, &["replay", &run, "2"]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("[done] COUNT"), "{output}");
    assert!(output.contains("  exit code: 0 before, 0 now"), "{output}");
    assert!(
        output.contains("every output has the same content as before"),
        "{output}"
    );
    assert!(
        output.contains("standard error now:\n  counted first"),
        "{output}"
    );
    let (code, output) = iunctus(path, &["runs"]);
    assert_eq!(code, 0, "{output}");
    assert!(
        output.contains(&format!("(replays task 2 of run {run})")),
        "{output}"
    );
}

#[test]
fn delivers_run_traces() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path();
    workflow(path);
    let (code, output) = iunctus(path, &["run", "main.nf"]);
    assert_eq!(code, 0, "{output}");
    let [run] = runs(path).try_into().expect("one run");
    let (endpoint, received) = receiver();
    let config = format!(
        "[telemetry]\nendpoint = \"{endpoint}\"\nheaders = {{ authorization = {{ environment = \
         \"IUNCTUS_TEST_OTLP\" }} }}\n"
    );
    std::fs::write(path.join("config.toml"), config).expect("writes the config");
    let arguments = [
        "telemetry",
        &run,
        "-output",
        "trace.jsonl",
        "-iunctus-config",
        "config.toml",
    ];
    let (code, output) = iunctus_with(path, &arguments, &[("IUNCTUS_TEST_OTLP", "Bearer t")]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains(&format!(
        "wrote the trace of run {run} to trace.jsonl: 5 spans"
    )));
    assert!(output.contains(&format!("sent the trace to {endpoint}/v1/traces: 5 spans")));
    let lines = std::fs::read_to_string(path.join("trace.jsonl")).expect("reads the trace");
    let line: serde_json::Value = serde_json::from_str(lines.trim()).expect("one JSON line");
    let spans = &line["resourceSpans"][0]["scopeSpans"][0]["spans"];
    assert_eq!(spans[0]["name"], "run main.nf");
    let (head, body) = received.recv().expect("a request");
    assert!(
        head.to_ascii_lowercase()
            .contains("authorization: bearer t"),
        "{head}"
    );
    assert_eq!(body, lines.trim());
    // A secret whose variable is not set stops the command before anything is sent.
    let (code, output) = iunctus(path, &arguments);
    assert_eq!(code, 1, "{output}");
    assert!(output.contains("IUNCTUS_TEST_OTLP"), "{output}");
}

/// A small OTLP HTTP receiver: it answers every request with 200 and hands the head and body of
/// each request to the channel.
fn receiver() -> (String, Receiver<(String, String)>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listens");
    let address = format!("http://{}", listener.local_addr().expect("an address"));
    let (sender, received) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let sender = sender.clone();
            std::thread::spawn(move || serve(stream, &sender));
        }
    });
    (address, received)
}

/// Answers the requests of one connection, which keeps going until the client closes it.
fn serve(stream: TcpStream, sender: &Sender<(String, String)>) {
    let mut reader = BufReader::new(stream.try_clone().expect("a second handle"));
    let mut writer = stream;
    loop {
        let mut head = String::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            if line == "\r\n" {
                break;
            }
            head.push_str(&line);
        }
        let length = head
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.trim().parse().ok())
            .unwrap_or(0);
        let mut body = vec![0; length];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        let _ = sender.send((head, String::from_utf8_lossy(&body).into_owned()));
        let reply =
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}";
        if writer.write_all(reply.as_bytes()).is_err() {
            return;
        }
    }
}
