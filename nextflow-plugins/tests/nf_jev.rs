//! nf-jev against a local fake of the Jev service, with the requests, answers, cache files and
//! messages that nf-jev 0.2.1 gave with Nextflow 26.04.6.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// The answer of the recorded fake for the three question kinds.
const ANSWER: &str = r#"{"model":"jev-1.13.0","answers":{"q1":{"type":"noul","noul":0.98},"q2":{"type":"choice","choice":"RNA-seq","confidence":0.99,"probabilities":{"RNA-seq":0.99,"ATAC-seq":0.01}},"q3":{"type":"score","score":1.97,"confidence":0.96,"legend":{"0":"low","1":"mid","2":"high"},"probabilities":{"0":0.0,"1":0.02,"2":0.98}}},"usage":{"requests":1}}"#;

/// A fake service that answers each request with this status and body; the bodies it heard.
fn service(status: &'static str, body: &'static str) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("binds a free port");
    let port = listener.local_addr().expect("an address").port();
    let heard = Arc::new(Mutex::new(Vec::new()));
    let log = heard.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(&stream);
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut received = vec![0; length];
            let _ = reader.read_exact(&mut received);
            log.lock()
                .expect("the log")
                .push(String::from_utf8_lossy(&received).into_owned());
            let answer = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = (&stream).write_all(answer.as_bytes());
        }
    });
    (port, heard)
}

/// Runs a script in `folder` with this `jev` scope; the error and what it printed.
fn run(folder: &Path, script: &str, scope: &str) -> (Option<String>, String) {
    std::fs::write(folder.join("main.nf"), script).expect("writes the script");
    let config = format!("plugins {{ id 'nf-jev@0.2.1' }}\njev {{ apiKey = 'dummy'; {scope} }}\n");
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, printed) = common::run(folder, "jev");
    (execution.error.map(|error| error.to_string()), printed)
}

#[test]
fn asks_and_caches() {
    let (port, heard) = service("200 OK", ANSWER);
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let fixtures = common::fixtures("nf_jev");
    let script = std::fs::read_to_string(fixtures.join("basic.nf")).expect("the script");
    let scope = format!(
        "endpoint = 'http://127.0.0.1:{port}/v1/systemone'; cacheDir = '{}/jev-cache'",
        folder.display()
    );
    let (error, printed) = run(folder, &script, &scope);
    assert!(error.is_none(), "{error:?}\n{printed}");
    let recorded = std::fs::read_to_string(fixtures.join("basic.txt")).expect("the record");
    let expected: Vec<&str> = (recorded.lines())
        .filter(|line| {
            !["###", "Downloading", "[SUCCESS]"]
                .iter()
                .any(|start| line.starts_with(start))
        })
        .map(|line| line.strip_prefix("[WARN] ").unwrap_or(line))
        .collect();
    let printed: Vec<&str> = printed.lines().collect();
    assert_eq!(printed, expected);
    let heard = heard.lock().expect("the log").clone();
    let requests =
        std::fs::read_to_string(fixtures.join("basic-requests.jsonl")).expect("the requests");
    let bodies: Vec<String> = (requests.lines())
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("JSON"))
        .map(|request| request["body"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        heard, bodies,
        "the same bytes, and the cache answers the repeated call"
    );
    let cache = folder
        .join("jev-cache/a9/47387fdf5b448e0fe2b84ae3979cab3c1f3555c327be1e7fd23a1d08761385.json");
    assert_eq!(std::fs::read_to_string(cache).ok().as_deref(), Some(ANSWER));
}

#[test]
fn reports_failures() {
    let one = "include { jev; noul; choice; score } from 'plugin/nf-jev'\nworkflow {\n  \
               println jev('s', [q1: noul('x')])\n}\n";
    let cases = [
        (
            "529 Busy",
            "{}",
            "Jev request failed after 3 attempts - status 529",
            3,
        ),
        (
            "400 Bad Request",
            r#"{"error":"busy"}"#,
            r#"Jev request failed with status 400 - {"error":"busy"}"#,
            1,
        ),
        (
            "200 OK",
            "not json",
            "Unable to parse the Jev response - not json",
            1,
        ),
        (
            "200 OK",
            r#"{"model":"m","result":1}"#,
            "Unexpected Jev response - expected an `answers` object, got: [model, result]",
            1,
        ),
    ];
    for (status, body, expected, requests) in cases {
        let (port, heard) = service(status, body);
        let directory = tempfile::tempdir().expect("a temporary directory");
        let scope = format!("endpoint = 'http://127.0.0.1:{port}/v1/systemone'");
        let (error, _) = run(directory.path(), one, &scope);
        let error = error.unwrap_or_default();
        assert!(error.contains(expected), "{status}: {error}");
        assert_eq!(heard.lock().expect("the log").len(), requests, "{status}");
    }
    let directory = tempfile::tempdir().expect("a temporary directory");
    let scope = "provider = 'typesafe'; endpoint = 'https://openrouter.ai/api/alpha/decisions'";
    let (error, _) = run(directory.path(), one, scope);
    let expected = "Conflicting Jev settings - `jev.provider` is `typesafe` but `jev.endpoint` points at \
                    OpenRouter (https://openrouter.ai/api/alpha/decisions); remove one of them";
    assert!(error.unwrap_or_default().contains(expected));
    let (error, _) = run(directory.path(), one, "provider = 'cloudflare'");
    assert!(
        error
            .unwrap_or_default()
            .contains("Unknown Jev provider `cloudflare`")
    );
    let closed = "endpoint = 'http://user:secret@127.0.0.1:9/x?key=hidden'";
    let (error, _) = run(directory.path(), one, closed);
    let error = error.unwrap_or_default();
    assert!(
        error.contains("Jev request failed after 3 attempts - "),
        "{error}"
    );
    assert!(
        !error.contains("secret") && !error.contains("hidden"),
        "{error}"
    );
    let builder = "include { noul; choice } from 'plugin/nf-jev'\nworkflow {\n  \
                   println noul('Seen?', [true: 'yes'])\n}\n";
    let (error, _) = run(
        directory.path(),
        builder,
        "endpoint = 'http://127.0.0.1:9/x'",
    );
    let expected = "The criteria of a noul question need a `true` and a `false` entry - got [true]";
    assert!(error.unwrap_or_default().contains(expected));
}
