//! What the tests that run the `uwe` binary share: a command that reads no config file of
//! the user and none of the site, so that results do not depend on the machine.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
#![allow(dead_code, reason = "each test file uses only a part of this module")]

use std::path::PathBuf;
use std::process::Command;

/// The path of the `uwe` binary that Cargo built for the tests.
pub const BINARY: &str = env!("CARGO_BIN_EXE_uwe");

/// The `uwe` binary with its own empty config folder and no site-wide file.
pub fn command() -> Command {
    isolated(Command::new(BINARY))
}

/// Sets the environment of `command` so that uwe reads no config file of the user (an
/// empty folder in place of `XDG_CONFIG_HOME`) and none of the site (`UWE_SITE_CONFIG` is
/// empty). A test that needs a config file sets `XDG_CONFIG_HOME` again after this.
pub fn isolated(mut command: Command) -> Command {
    command
        .env("XDG_CONFIG_HOME", empty_config_folder())
        .env(config_file::SITE_FILE_VARIABLE, "");
    command
}

/// A folder without any config file, made once below the target folder of the tests.
fn empty_config_folder() -> PathBuf {
    let folder = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("empty-config");
    std::fs::create_dir_all(&folder).expect("makes the empty config folder");
    folder
}

/// A small web server on 127.0.0.1 for tests: it serves files from memory with an ETag that
/// changes with their content, and the files can change while it runs.
pub struct WebServer {
    /// The address of the server, like `http://127.0.0.1:4711`.
    pub address: String,
    files: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
}

impl WebServer {
    pub fn start(files: &[(&str, &str)]) -> WebServer {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
        let address = format!("http://{}", listener.local_addr().expect("an address"));
        let files: std::collections::HashMap<String, String> = (files.iter())
            .map(|(path, body)| (path.to_string(), body.to_string()))
            .collect();
        let served = std::sync::Arc::new(std::sync::Mutex::new(files));
        let files = served.clone();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().expect("a stream"));
                let mut first = String::new();
                let _ = reader.read_line(&mut first);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                }
                let mut parts = first.split_whitespace();
                let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                let body = served.lock().expect("the files").get(path).cloned();
                let (status, body) = match body {
                    Some(body) => ("200 OK", body),
                    None => ("404 Not Found", String::new()),
                };
                let tag = body.bytes().fold(body.len() as u64, |sum, byte| {
                    sum.wrapping_mul(31).wrapping_add(u64::from(byte))
                });
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nETag: \"{tag:x}\"\r\nConnection: \
                     close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                if method != "HEAD" {
                    let _ = stream.write_all(body.as_bytes());
                }
            }
        });
        WebServer { address, files }
    }

    /// Serves `body` at `path` from now on.
    pub fn set(&self, path: &str, body: &str) {
        let mut files = self.files.lock().expect("the files");
        files.insert(path.to_owned(), body.to_owned());
    }
}

/// The output of a real run without what the run shows of itself in any language: its first
/// line, like `uwe 0.1.0: running main.nf (Nextflow) on local, run <id>`, and its steps
/// and summary at its end, from `executor >` to `More:`.
pub fn without_display(output: &str) -> String {
    let header = format!("uwe {}: running ", env!("CARGO_PKG_VERSION"));
    let mut kept: Vec<&str> = Vec::new();
    let mut summary = false;
    for line in output.lines() {
        if line.starts_with("executor >  ") {
            summary = true;
        }
        if !summary && !line.starts_with(&header) && !line.starts_with("uwe: simulated run ") {
            kept.push(line);
        }
        if line.starts_with("More: `uwe runs`") {
            summary = false;
        }
    }
    kept.iter().map(|line| format!("{line}\n")).collect()
}

/// The workflow of a compiled document as one native op that runs it with the driver of its
/// language, the native baseline of a compiled run. The entry graph keeps its ports.
pub fn native_document(
    compiled: &serde_json::Value,
    source: serde_json::Value,
) -> serde_json::Value {
    use serde_json::{Value, json};
    let entry = (compiled["graphs"].as_array().into_iter().flatten())
        .find(|graph| graph["name"] == compiled["entry"])
        .expect("an entry graph");
    let ports = |key: &str| entry[key].as_array().cloned().unwrap_or_default();
    let (inputs, outputs) = (ports("inputs"), ports("outputs"));
    let typed = |ports: &[Value]| -> Vec<Value> {
        let typed = ports
            .iter()
            .map(|port| json!({"name": port["name"], "type": port["type"]}));
        typed.collect()
    };
    let mut edges: Vec<Value> = (inputs.iter())
        .map(|port| json!({"from": {"port": port["name"]}, "to": {"op": "native", "port": port["name"]}}))
        .collect();
    edges.extend(outputs.iter().map(|port| {
        json!({"from": {"op": "native", "port": port["name"]}, "to": {"port": port["name"]}})
    }));
    let mut native = json!({"source": source["id"], "language": source["language"]});
    for key in ["entry", "profiles"] {
        if let Some(value) = source.get(key) {
            native[key] = value.clone();
        }
    }
    let mut source = source;
    if let Some(fields) = source.as_object_mut() {
        fields.retain(|key, _| !["entry", "profiles"].contains(&key.as_str()));
    }
    let op = json!({
        "path": "native",
        "kind": {"native": native},
        "inputs": typed(&inputs),
        "outputs": typed(&outputs),
    });
    let graph =
        json!({"name": "main", "inputs": inputs, "outputs": outputs, "ops": [op], "edges": edges});
    json!({"wir_version": compiled["wir_version"], "sources": [source], "graphs": [graph], "entry": "main"})
}
