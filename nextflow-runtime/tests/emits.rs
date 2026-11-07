//! What the entry workflow of a Nextflow run emits: the values of its channels as JSON.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::json;
use workflow_graph::{Driver, Launch, Simulator, execute};

const SCRIPT: &str = r#"
process GREET {
    input:
    val name
    output:
    path "${name}.txt"
    script:
    """
    echo hello ${name} > ${name}.txt
    """
}

workflow greetings {
    main:
    GREET(channel.of('ada', 'bob'))
    emit:
    files = GREET.out
    count = GREET.out.count()
    label = 'greetings'
    names = channel.of([id: 'ada', size: 2])
}

workflow {
    greetings()
}
"#;

#[test]
fn records_entry_emits() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("main.nf"), SCRIPT).unwrap();
    let launch = Launch {
        target: directory.path().join("main.nf"),
        targets: vec!["greetings".into()],
        simulated: true,
        ..Launch::default()
    };
    let settings = nextflow_runtime::driver::Settings {
        host: Box::new(Silent),
        ..Default::default()
    };
    let mut driver = nextflow_runtime::driver::Nextflow::new(&launch, settings).unwrap();
    assert_eq!(driver.emitted().unwrap(), None, "nothing before the start");
    let mut simulator = Simulator::new(directory.path().join("simulator"));
    execute("emits", &mut driver, &mut simulator)
        .into_result()
        .unwrap();
    let emitted = driver.emitted().unwrap().unwrap();
    let files = emitted["files"].as_array().unwrap();
    let mut names: Vec<&str> = files
        .iter()
        .map(|file| file["basename"].as_str().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["ada.txt", "bob.txt"], "{emitted}");
    assert!(files.iter().all(|file| file["class"] == "File"));
    assert_eq!(emitted["count"], 2);
    assert_eq!(emitted["label"], "greetings");
    assert_eq!(emitted["names"], json!([{"id": "ada", "size": 2}]));
    assert_eq!(driver.results(), None, "a simulated run has no results");
}

#[test]
fn unnamed_entry_emits_nothing() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("main.nf"), SCRIPT).unwrap();
    let launch = Launch {
        target: directory.path().join("main.nf"),
        simulated: true,
        ..Launch::default()
    };
    let settings = nextflow_runtime::driver::Settings {
        host: Box::new(Silent),
        ..Default::default()
    };
    let mut driver = nextflow_runtime::driver::Nextflow::new(&launch, settings).unwrap();
    let mut simulator = Simulator::new(directory.path().join("simulator"));
    let graph = execute("emits", &mut driver, &mut simulator)
        .into_result()
        .unwrap();
    assert_eq!(graph.nodes.len(), 2, "the same tasks run");
    assert_eq!(driver.emitted().unwrap(), Some(json!({})));
}

/// A host that prints nothing.
struct Silent;

impl nextflow_evaluator::interpreter::Host for Silent {
    fn print(&mut self, _: &str) {}
}
