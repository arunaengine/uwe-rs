//! Runs load ops: a real run reads the files, and a file that is missing fails the op.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::{Value as Json, json};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Driver, Launch, Simulator, execute};

/// A real run of a document with one load op over the File input `text` at `location`.
fn run(root: &Path, location: &str) -> Result<Json, String> {
    let file = json!({"file": {}});
    let document = json!({
        "wir_version": wir::VERSION,
        "graphs": [{
            "name": "main",
            "inputs": [{"name": "text", "type": file}],
            "outputs": [{"name": "out", "type": file}],
            "ops": [{
                "path": "read",
                "kind": {"load": {"contents": true}},
                "inputs": [{"name": "text", "type": file}],
                "outputs": [{"name": "out", "type": file}]
            }],
            "edges": [
                {"from": {"port": "text"}, "to": {"op": "read", "port": "text"}},
                {"from": {"op": "read", "port": "out"}, "to": {"port": "out"}}
            ]
        }],
        "entry": "main"
    });
    std::fs::write(root.join("load.wir.json"), document.to_string()).unwrap();
    let inputs = json!({"text": {"class": "File", "location": location}});
    std::fs::write(root.join("inputs.json"), inputs.to_string()).unwrap();
    let launch = Launch {
        target: root.join("load.wir.json"),
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
    let execution = execute("load", &mut driver, &mut simulator);
    execution.into_result().map_err(|error| error.to_string())?;
    Ok(driver.results().unwrap()["out"].clone())
}

#[test]
fn reads_real_files() {
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(folder.path().join("a.txt"), "hello\n").unwrap();
    let out = run(folder.path(), "a.txt").unwrap();
    assert_eq!(out["contents"], "hello\n");
    let error = run(folder.path(), "missing.txt").unwrap_err();
    assert!(
        error.contains("cannot read"),
        "a real run reads no made-up file: {error}"
    );
}
