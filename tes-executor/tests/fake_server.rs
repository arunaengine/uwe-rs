//! The TES backend against a small fake server on this machine, which answers as Funnel does
//! and writes the files that the task would give back.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use executor::registry::{ImageConfig, ImageConfigs};
use executor::{Backend, Job, Staged, Staging};
use task_model::{FORMAT, Image, OutputRule, Placement, TaskSpec};
use tes_executor::{Client, TesBackend};
use tokio_util::sync::CancellationToken;

/// Reads one request: its first line and its body.
fn request(stream: &mut std::net::TcpStream) -> (String, String) {
    let mut reader = BufReader::new(stream);
    let mut first = String::new();
    reader.read_line(&mut first).expect("a request line");
    let mut length = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("a header");
        if line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).expect("a body");
    (first, String::from_utf8_lossy(&body).into_owned())
}

/// Writes what a container would leave for an output of the task: the control files, or
/// `out.txt` in the outbox.
fn leave(output: &serde_json::Value) {
    let url = output["url"].as_str().expect("a URL");
    let folder = PathBuf::from(url.strip_prefix("file://").expect("a file URL"));
    std::fs::create_dir_all(&folder).expect("makes the folder");
    match output["path"].as_str() {
        Some("/iunctus/control") => {
            std::fs::write(folder.join("exit"), "0\n").expect("writes");
            std::fs::write(folder.join("manifest"), "file out.txt\n").expect("writes");
        }
        _ => std::fs::write(folder.join("out.txt"), "hi\n").expect("writes"),
    }
}

/// A server that takes one task, gives back `out.txt` and exit code 0 when asked about it,
/// and keeps the task it got.
fn serve(task: Arc<Mutex<Option<serde_json::Value>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let address = format!("http://{}", listener.local_addr().expect("an address"));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let (first, body) = request(&mut stream);
            let answer = if first.starts_with("POST /ga4gh/tes/v1/tasks ") {
                let document: serde_json::Value = serde_json::from_str(&body).expect("JSON");
                for output in document["outputs"].as_array().expect("outputs") {
                    leave(output);
                }
                *task.lock().expect("the task") = Some(document);
                r#"{"id":"task-1"}"#.to_owned()
            } else {
                r#"{"id":"task-1","state":"COMPLETE"}"#.to_owned()
            };
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{answer}",
                answer.len()
            );
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    address
}

#[test]
fn tasks_run_on_the_server_and_give_back_their_outputs() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let task = Arc::new(Mutex::new(None));
    let address = serve(task.clone());
    // The image runs commands through an entrypoint, as Biocontainers do.
    let config = ImageConfig {
        entrypoint: vec!["/usr/local/env-execute".into()],
        cmd: Vec::new(),
    };
    let backend = TesBackend {
        client: Client::new(&address, None).expect("a client"),
        scratch: folder.path().join("scratch"),
        configs: ImageConfigs::known([("debian:stable-slim".to_owned(), config)]),
    };
    let source = folder.path().join("in.txt");
    std::fs::write(&source, "in\n").expect("writes the input");
    let spec = TaskSpec {
        format: FORMAT,
        namespace: String::new(),
        image: Some(Image {
            reference: "debian:stable-slim".into(),
            platform: None,
        }),
        arguments: vec!["bash".into(), "-c".into(), "cp in.txt out.txt".into()],
        workdir: None,
        environment: Default::default(),
        inputs: Vec::new(),
        outputs: vec![OutputRule {
            pattern: "out.txt".into(),
        }],
        stdin: None,
    };
    let staging = Staging {
        inputs: vec![Staged {
            path: "in.txt".into(),
            source,
            directory: false,
            copy: false,
            executable: false,
            ..Default::default()
        }],
        ..Staging::default()
    };
    let job = Job {
        name: "run-P-1-1",
        spec: &spec,
        staging: &staging,
        placement: &Placement::default(),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let finished = runtime
        .block_on(backend.run(job, &CancellationToken::new()))
        .expect("runs");
    assert_eq!(finished.exit_code, Some(0));
    assert_eq!(finished.files.len(), 1);
    let output = std::fs::read_to_string(finished.local_path("out.txt")).expect("reads");
    assert_eq!(output, "hi\n");
    let sent = task.lock().expect("the task").clone().expect("a task");
    assert_eq!(sent["executors"][0]["command"][1], "/iunctus/wrapper.sh");
    let wrapper = std::fs::read_to_string(finished.root.join("wrapper.sh")).expect("reads");
    assert!(
        wrapper.contains("'/usr/local/env-execute' 'bash' '-c' 'cp in.txt out.txt'"),
        "{wrapper}"
    );
    assert_eq!(sent["inputs"][1]["path"], "/iunctus/work/in.txt");
}
