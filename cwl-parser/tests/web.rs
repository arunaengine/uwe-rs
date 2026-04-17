//! Tests reading documents from the web, as for a `run` target given by a web address.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

use cwl_parser::Loader;
use cwl_parser::source::{Source, Web};

const TOOL: &str = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: cat\n\
                    inputs:\n  file: File\noutputs:\n  out: stdout\n";

/// Serves `text` over HTTP on a local port for a number of requests; gives the address.
fn serve(text: &'static str, requests: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
    let address = listener.local_addr().expect("the address of the port");
    std::thread::spawn(move || {
        for stream in listener.incoming().take(requests) {
            let Ok(mut stream) = stream else { continue };
            let Ok(copy) = stream.try_clone() else {
                continue;
            };
            let mut reader = BufReader::new(copy);
            let mut line = String::new();
            // The request ends with an empty line.
            while reader.read_line(&mut line).is_ok_and(|read| read > 2) {
                line.clear();
            }
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                text.len()
            );
        }
    });
    format!("http://{address}")
}

#[test]
fn reads_web_addresses() {
    let address = serve(TOOL, 1);
    let text = Web.read(&format!("{address}/tool.cwl"));
    assert_eq!(text.as_deref(), Ok(TOOL));
}

#[test]
fn loads_run_targets_from_the_web() {
    let address = serve(TOOL, 1);
    let folder = tempfile::tempdir().expect("a temporary folder");
    let workflow = folder.path().join("workflow.cwl");
    let text = format!(
        "cwlVersion: v1.2\nclass: Workflow\ninputs: {{file: File}}\n\
         outputs:\n  out: {{type: File, outputSource: cat/out}}\nsteps:\n  \
         cat: {{run: '{address}/tool.cwl', in: {{file: file}}, out: [out]}}\n"
    );
    std::fs::write(&workflow, text).expect("writes the workflow");
    let document = Loader::new(Web)
        .load(&workflow.to_string_lossy())
        .unwrap_or_else(|error| panic!("{error}"));
    let cwl_parser::model::Class::Workflow(model) = &document.process.class else {
        panic!("not a workflow");
    };
    assert_eq!(model.steps[0].run.process.id, format!("{address}/tool.cwl"));
}
