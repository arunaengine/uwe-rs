//! Runs tasks whose images come from a fake Wave service, as nf-wave resolves them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use workflow_graph::Launch;

/// The script of the recording: one process with an image, one with Conda packages.
const SCRIPT: &str = "process WITH_IMAGE {\n container 'ubuntu:22.04'\n script:\n 'echo image'\n}\n\
                      process WITH_CONDA {\n conda 'bioconda::samtools=1.17'\n script:\n 'echo conda'\n}\n\
                      workflow {\n WITH_IMAGE()\n WITH_CONDA()\n}\n";

/// Answers each container request with a target image named after its number; the bodies.
fn service() -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let base = format!("http://{}", listener.local_addr().expect("an address"));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let heard = bodies.clone();
    std::thread::spawn(move || {
        for (index, stream) in listener.incoming().flatten().enumerate() {
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
            let mut body = vec![0; length];
            let _ = reader.read_exact(&mut body);
            let body = serde_json::from_slice(&body).unwrap_or_default();
            heard.lock().expect("the bodies").push(body);
            let reply = serde_json::json!({
                "requestId": format!("req{index}"),
                "targetImage": format!("wave.example/wt/{index}/image:fake"),
                "succeeded": true,
            })
            .to_string();
            let answer = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            let _ = (&stream).write_all(answer.as_bytes());
        }
    });
    (base, bodies)
}

#[test]
fn takes_wave_images() {
    let (base, bodies) = service();
    let directory = tempfile::tempdir().expect("a folder");
    std::fs::write(directory.path().join("main.nf"), SCRIPT).expect("writes the script");
    let config = format!(
        "docker.enabled = true\nwave {{\n enabled = true\n endpoint = '{base}'\n httpClient.maxRate = '100/sec'\n}}\n"
    );
    std::fs::write(directory.path().join("nextflow.config"), config).expect("writes the config");
    let mut driver = nextflow_runtime::driver(&Launch {
        target: directory.path().to_owned(),
        ..Launch::default()
    })
    .expect("a driver");
    let tasks = driver.start().expect("starts");
    let mut images: Vec<&str> = tasks
        .iter()
        .filter_map(|task| task.image.as_deref())
        .collect();
    images.sort();
    assert_eq!(
        images,
        [
            "wave.example/wt/0/image:fake",
            "wave.example/wt/1/image:fake"
        ]
    );
    let bodies = bodies.lock().expect("the bodies");
    let image = bodies
        .iter()
        .find(|body| body["containerImage"] == "ubuntu:22.04");
    assert_eq!(
        image.map(|body| &body["dryRun"]),
        Some(&serde_json::json!(false))
    );
    let conda = bodies.iter().find(|body| body["packages"].is_object());
    let entries = conda.map(|body| &body["packages"]["entries"]);
    assert_eq!(
        entries,
        Some(&serde_json::json!(["bioconda::samtools=1.17"]))
    );
}
