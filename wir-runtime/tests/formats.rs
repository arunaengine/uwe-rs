//! Checks the formats of input Files by the prefixes and ontologies of a document.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::{Value as Json, json};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Launch, Simulator, execute};

const ONTOLOGY: &str = "@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
<http://example.org/narrow> rdfs:subClassOf <http://example.org/fasta> .
<http://example.org/fa> owl:equivalentClass <http://example.org/narrow> .
";

/// A document whose task reads a File of the format `fasta`, with the ontology above, run on
/// a File of `format`.
fn launch(root: &Path, format: &str) -> Launch {
    let file = json!({"file": {"format": "http://example.org/fasta"}});
    let task =
        json!({"task": {"image": "debian:stable-slim", "command": ["wc", {"input": "text"}]}});
    let document = json!({
        "wir_version": wir::VERSION,
        "graphs": [{
            "name": "main",
            "inputs": [{"name": "text", "type": file}],
            "ops": [{"path": "count", "kind": task, "inputs": [{"name": "text", "type": file}]}],
            "edges": [{"from": {"port": "text"}, "to": {"op": "count", "port": "text"}}]
        }],
        "entry": "main",
        "formats": {"prefixes": {"ex": "http://example.org/"}, "ontologies": ["formats.ttl"]}
    });
    std::fs::write(root.join("formats.wir.json"), document.to_string()).unwrap();
    std::fs::write(root.join("formats.ttl"), ONTOLOGY).unwrap();
    std::fs::write(root.join("text.fa"), ">a\nAC\n").unwrap();
    let text = json!({"class": "File", "location": "text.fa", "format": format});
    std::fs::write(root.join("inputs.json"), json!({"text": text}).to_string()).unwrap();
    Launch {
        target: root.join("formats.wir.json"),
        inputs: Some(root.join("inputs.json")),
        scratch: root.join("scratch"),
        simulated: true,
        ..Launch::default()
    }
}

/// Why a run on a File of `format` failed, if it did.
fn run(format: &str) -> Option<String> {
    let folder = tempfile::tempdir().unwrap();
    let settings = Settings {
        print: false,
        ..Settings::default()
    };
    let mut driver = WirDriver::new(&launch(folder.path(), format), settings).unwrap();
    let mut simulator = Simulator::new(folder.path().join("simulator"));
    let execution = execute("formats", &mut driver, &mut simulator);
    execution.into_result().err().map(|error| error.to_string())
}

#[test]
fn matches_ontology_formats() {
    // Equal text, a subclass, and an equivalent class of a subclass, prefixed or not.
    for format in ["ex:fasta", "http://example.org/narrow", "ex:fa"] {
        assert_eq!(run(format), None, "{format}");
    }
    let error = run("ex:fastq").expect("another format fails");
    assert!(error.contains("does not fit its type"), "{error}");
}

#[test]
fn verifies_unequal_formats() {
    let folder = tempfile::tempdir().unwrap();
    let launch = launch(folder.path(), "ex:fa");
    let text = std::fs::read_to_string(&launch.target).unwrap();
    let mut document: Json = serde_json::from_str(&text).unwrap();
    // A run tells by the ontology whether a narrower format fits a wider port.
    document["graphs"][0]["inputs"][0]["type"] =
        json!({"file": {"format": "http://example.org/narrow"}});
    let parsed: wir::Document = serde_json::from_value(document.clone()).unwrap();
    assert!(wir::verify(&parsed).is_ok());
    document["formats"]["ontologies"] = json!([]);
    let parsed: wir::Document = serde_json::from_value(document).unwrap();
    assert!(
        wir::verify(&parsed).is_err(),
        "equal text without ontologies"
    );
}
