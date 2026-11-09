//! Finds the secondary files of web inputs, as uwe's CWL driver does: a local web server has
//! the primary file and its required index, and no optional one. A File lists none as empty.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{BufRead, BufReader, Write};

use serde_json::json;
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Driver, Launch, Simulator, execute};

/// A web server on a local port with `/a.bam` and `/a.bam.bai`; other paths are missing.
fn serve() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            let _ = reader.read_line(&mut first);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 2 {
                line.clear();
            }
            let status = match first.split(' ').nth(1) {
                Some("/a.bam" | "/a.bam.bai") => "200 OK",
                _ => "404 Not Found",
            };
            let head =
                format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            let _ = stream.write_all(head.as_bytes());
        }
    });
    address
}

#[test]
fn finds_web_secondaries() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let secondary = json!([{"pattern": ".bai"}, {"pattern": ".csi", "required": false}]);
    let reads = json!({"file": {"secondary_files": secondary}});
    let document = json!({
        "wir_version": wir::VERSION,
        "graphs": [{
            "name": "main",
            "inputs": [{"name": "reads", "type": reads}],
            "outputs": [{"name": "out", "type": reads}],
            "ops": [],
            "edges": [{"from": {"port": "reads"}, "to": {"port": "out"}}]
        }],
        "entry": "main"
    });
    std::fs::write(root.join("pass.wir.json"), document.to_string()).unwrap();
    let inputs = json!({"reads": {"class": "File", "location": format!("{}/a.bam", serve())}});
    std::fs::write(root.join("inputs.json"), inputs.to_string()).unwrap();
    let launch = Launch {
        target: root.join("pass.wir.json"),
        inputs: Some(root.join("inputs.json")),
        scratch: root.join("scratch"),
        output_dir: Some(root.join("out")),
        ..Launch::default()
    };
    let settings = Settings {
        print: false,
        ..Settings::default()
    };
    let mut driver = WirDriver::new(&launch, settings).unwrap();
    let mut simulator = Simulator::new(root.join("simulator"));
    execute("pass", &mut driver, &mut simulator)
        .into_result()
        .unwrap();
    let found = &driver.results().unwrap()["out"]["secondary_files"];
    assert_eq!(
        found.as_array().map(Vec::len),
        Some(1),
        "only the index: {found}"
    );
    let delivered = found[0]["location"].as_str().unwrap();
    assert!(delivered.ends_with("/a.bam.bai"), "{delivered}");
}

#[test]
fn lists_no_secondaries() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let indexed = json!({"file": {"secondary_files": [{"pattern": ".fai", "required": false}]}});
    let found = json!({"array": {"items": {"file": {}}}});
    let document = json!({
        "wir_version": wir::VERSION,
        "graphs": [{
            "name": "main",
            "inputs": [{"name": "genome", "type": {"file": {}}}],
            "outputs": [{"name": "out", "type": found}],
            "ops": [{
                "path": "index",
                "kind": {"expr": {"language": "js", "code": "$(inputs.genome.secondaryFiles)"}},
                "inputs": [{"name": "genome", "type": indexed}],
                "outputs": [{"name": "out", "type": found}]
            }],
            "edges": [
                {"from": {"port": "genome"}, "to": {"op": "index", "port": "genome"}},
                {"from": {"op": "index", "port": "out"}, "to": {"port": "out"}}
            ]
        }],
        "entry": "main"
    });
    std::fs::write(root.join("index.wir.json"), document.to_string()).unwrap();
    std::fs::write(root.join("genome.fa"), ">a\nACGT\n").unwrap();
    let inputs = json!({"genome": {"class": "File", "location": "genome.fa"}});
    std::fs::write(root.join("inputs.json"), inputs.to_string()).unwrap();
    let launch = Launch {
        target: root.join("index.wir.json"),
        inputs: Some(root.join("inputs.json")),
        scratch: root.join("scratch"),
        output_dir: Some(root.join("out")),
        ..Launch::default()
    };
    let settings = Settings {
        print: false,
        ..Settings::default()
    };
    let mut driver = WirDriver::new(&launch, settings).unwrap();
    let mut simulator = Simulator::new(root.join("simulator"));
    execute("index", &mut driver, &mut simulator)
        .into_result()
        .unwrap();
    let out = &driver.results().unwrap()["out"];
    assert_eq!(
        out,
        &json!([]),
        "an op that asks for secondary files sees an empty list"
    );
}
