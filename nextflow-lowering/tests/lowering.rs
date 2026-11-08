//! Small pipelines lower into call graphs, process ops with Nextflow dialect tasks, stream ops
//! with Groovy callbacks and the ops of the start of the run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_lowering::Lowering;
use nextflow_runtime::driver::Settings;
use serde_json::{Value as Json, json};
use workflow_graph::Launch;

/// A host in the folder of the test that prints nothing.
struct Quiet(String);

impl nextflow_evaluator::interpreter::Host for Quiet {
    fn print(&mut self, _: &str) {}

    fn launch_dir(&self) -> String {
        self.0.clone()
    }
}

/// Lowers a script with the profile `test`.
fn lower(script: &str) -> Lowering {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("main.nf"), script).unwrap();
    let config = "profiles {\n    test {\n        params.greeting = 'hi'\n    }\n}\n";
    std::fs::write(directory.path().join("nextflow.config"), config).unwrap();
    let launch = Launch {
        target: directory.path().to_owned(),
        scratch: directory.path().join("scratch"),
        profiles: vec!["test".into()],
        ..Launch::default()
    };
    let settings = Settings {
        host: Box::new(Quiet(directory.path().to_string_lossy().into_owned())),
        ..Settings::default()
    };
    nextflow_lowering::lower(&launch, settings, directory.path()).unwrap()
}

fn full(script: &str) -> Json {
    let lowering = lower(script);
    assert!(lowering.full(), "nothing stays native: {:?}", lowering.kept);
    lowering.document
}

fn graph<'a>(document: &'a Json, name: &str) -> &'a Json {
    let graphs = document["graphs"].as_array().unwrap();
    let found = graphs.iter().find(|graph| graph["name"] == name);
    found.unwrap_or_else(|| panic!("graph {name}"))
}

fn op<'a>(graph: &'a Json, path: &str) -> &'a Json {
    let ops = graph["ops"].as_array().unwrap();
    let found = ops.iter().find(|op| op["path"] == path);
    found.unwrap_or_else(|| panic!("op {path} in {graph}"))
}

#[test]
fn keeps_unbound_values() {
    let lowering = lower("workflow {\n    channel.value().view()\n}\n");
    assert_eq!(lowering.kept.len(), 1, "{:?}", lowering.kept);
    assert!(
        lowering.kept[0].reason.contains("binds"),
        "{:?}",
        lowering.kept
    );
    let main = graph(&lowering.document, "main");
    assert_eq!(
        op(main, "pipeline")["kind"]["native"]["profiles"],
        json!(["test"])
    );
    let document = full("workflow {\n}\n");
    let main = graph(&document, "main");
    let params = json!({"name": "params", "type": "dynamic", "default": {"greeting": "hi"}});
    assert_eq!(main["inputs"], json!([params]));
}
