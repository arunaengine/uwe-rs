//! Checks ontology relationships through CWL document loading and input admission.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_runtime::{CwlDriver, Options};
use workflow_graph::{Driver, Launch};

#[test]
fn checks_ontology_formats() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("formats.ttl"),
        r#"
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
<http://example/child> rdfs:subClassOf <http://example/parent> .
<http://example/parent> rdfs:subClassOf <http://example/root> .
<http://example/alias> owl:equivalentClass <http://example/child> .
"#,
    )
    .unwrap();
    std::fs::write(
        root.path().join("main.cwl"),
        r#"
cwlVersion: v1.2
class: CommandLineTool
$schemas: [formats.ttl]
baseCommand: cat
inputs: {data: {type: File, format: 'http://example/root'}}
outputs: []
"#,
    )
    .unwrap();
    for (format, accepted) in [("child", true), ("alias", true), ("unknown", false)] {
        std::fs::write(
            root.path().join("job.json"),
            serde_json::json!({
                "data": {"class": "File", "location": "file:///placeholder.txt",
                    "format": format!("http://example/{format}")}
            })
            .to_string(),
        )
        .unwrap();
        let mut driver = CwlDriver::new(
            &Launch {
                target: root.path().join("main.cwl"),
                inputs: Some(root.path().join("job.json")),
                ..Launch::default()
            },
            Options::default(),
        )
        .unwrap();
        let tasks = driver.start();
        assert_eq!(tasks.is_ok(), accepted, "{format}: {tasks:?}");
        if let Err(error) = tasks {
            assert!(!error.unsupported);
            assert!(error.message.contains("has the format"));
        }
    }
}
