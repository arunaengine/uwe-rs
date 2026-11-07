//! Runs expr ops: parameter references, File fields as CWL shows them, values that do not fit,
//! and JavaScript with a library.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::{Value as Json, json};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Driver, Launch, Simulator, execute};

/// A document with one expr op `value` of `code` over the inputs `text` and `count`.
fn run(root: &Path, code: &str, output: Json) -> Result<Json, String> {
    let ports = json!([{"name": "text", "type": {"file": {}}}, {"name": "count", "type": "int"}]);
    let edges: Vec<Json> = ["text", "count"]
        .iter()
        .map(|port| json!({"from": {"port": port}, "to": {"op": "value", "port": port}}))
        .chain([json!({"from": {"op": "value", "port": "out"}, "to": {"port": "out"}})])
        .collect();
    let document = json!({
        "wir_version": wir::VERSION,
        "graphs": [{
            "name": "main",
            "inputs": ports,
            "outputs": [{"name": "out", "type": output}],
            "ops": [{
                "path": "value",
                "kind": {"expr": {
                    "language": "js", "code": code, "library": ["function twice(n) { return 2 * n; }"]
                }},
                "inputs": ports,
                "outputs": [{"name": "out", "type": output}]
            }],
            "edges": edges
        }],
        "entry": "main"
    });
    std::fs::write(root.join("expr.wir.json"), document.to_string()).unwrap();
    std::fs::write(root.join("reads.fastq.gz"), "@r\n").unwrap();
    let inputs = json!({"text": {"class": "File", "location": "reads.fastq.gz"}, "count": 3});
    std::fs::write(root.join("inputs.json"), inputs.to_string()).unwrap();
    let launch = Launch {
        target: root.join("expr.wir.json"),
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
    let execution = execute("expr", &mut driver, &mut simulator);
    execution.into_result().map_err(|error| error.to_string())?;
    Ok(driver.results().unwrap()["out"].clone())
}

#[test]
fn evaluates_references() {
    let folder = tempfile::tempdir().unwrap();
    let out = run(folder.path(), "$(inputs.text.nameroot)", json!("string")).unwrap();
    assert_eq!(out, "reads.fastq", "a File shows its CWL fields");
    let out = run(folder.path(), "$(inputs.count)", json!("float")).unwrap();
    assert_eq!(out, json!(3.0), "an int widens to a float");
    let out = run(folder.path(), "n=$(inputs.count)", json!("string")).unwrap();
    assert_eq!(out, "n=3");
}

#[test]
fn refuses_unfit_values() {
    let folder = tempfile::tempdir().unwrap();
    let error = run(folder.path(), "$(inputs.count)", json!("string")).unwrap_err();
    assert_eq!(error, "op value: the value 3 does not fit output out");
}

#[test]
#[ignore = "needs the CWL JavaScript helper image, which `just test` builds"]
fn evaluates_javascript() {
    let folder = tempfile::tempdir().unwrap();
    let out = run(folder.path(), "$(twice(inputs.count) + 0.0)", json!("int")).unwrap();
    assert_eq!(out, json!(6), "a whole number fits an int");
    let body = "${ return inputs.text.basename.split('.').length; }";
    assert_eq!(run(folder.path(), body, json!("int")).unwrap(), json!(3));
}
