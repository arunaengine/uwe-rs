//! nf-synapse: the endpoint and token of the `synapse` scope reach the Synapse provider that
//! reads `syn://` paths.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

use nextflow_config::Config;
use nextflow_evaluator::interpreter::Host;
use nextflow_evaluator::value::Value;
use nextflow_plugins::catalogue;

struct Quiet;

impl Host for Quiet {
    fn print(&mut self, _: &str) {}
}

fn loaded(text: &str) -> Config {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let file = folder.path().join("nextflow.config");
    std::fs::write(&file, text).expect("writes");
    let options = nextflow_config::Options {
        profiles: None,
        params: Value::map([]),
        project_dir: "/project".into(),
        launch_dir: "/launch".into(),
        work_dir: "/work".into(),
    };
    nextflow_config::load(&[file], &options, &mut Quiet).expect("loads")
}

#[test]
fn scope_reaches_storage() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let endpoint = format!("http://{}", listener.local_addr().expect("an address"));
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("a request");
        let mut reader = BufReader::new(stream.try_clone().expect("a stream"));
        let mut lines = Vec::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).expect("a line");
            if line.trim().is_empty() {
                break;
            }
            lines.push(line.trim().to_owned());
        }
        let body = "{\"id\":\"syn100\",\"concreteType\":\"org.sagebionetworks.repo.model.Folder\"}";
        let answer = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        (&stream).write_all(answer.as_bytes()).expect("an answer");
        lines
    });
    let config = loaded(&format!(
        "plugins {{ id 'nf-synapse@0.1.3' }}\nsynapse {{\n    endpoint = '{endpoint}'\n    \
         authToken = 'fake-token'\n}}\n"
    ));
    let observers = catalogue::observers(&config).expect("an activated plugin");
    assert!(observers.is_empty());
    let storage = storage::Storage::default().blocking();
    let found = storage.stat("syn://syn100").expect("a stat");
    assert!(found.expect("an entity").directory);
    let lines = server.join().expect("the server");
    assert_eq!(lines[0], "GET /repo/v1/entity/syn100 HTTP/1.1");
    let token = "authorization: Bearer fake-token";
    assert!(
        lines.iter().any(|line| line.eq_ignore_ascii_case(token)),
        "{lines:?}"
    );
}
