//! Workflow outputs of compiled pipelines publish the files of tasks by their names in the
//! task folders, as the native driver does.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use nextflow_runtime::driver::Settings;
use workflow_graph::{Engine, Event, Launch, Observer, Simulator};

/// A host in the folder of the test that prints nothing.
struct Quiet(String);

impl nextflow_evaluator::interpreter::Host for Quiet {
    fn print(&mut self, _: &str) {}

    fn launch_dir(&self) -> String {
        self.0.clone()
    }
}

fn settings(folder: &Path) -> Settings {
    Settings {
        host: Box::new(Quiet(folder.to_string_lossy().into_owned())),
        ..Settings::default()
    }
}

/// The targets of the deliveries of a run.
#[derive(Default)]
struct Targets(Vec<String>);

impl Observer for Targets {
    fn event(&mut self, event: Event<'_>) {
        if let Event::Delivered { delivery } = event {
            self.0.push(delivery.target.clone());
        }
    }
}

/// The delivery targets of a run of the compiled pipeline `script` on the simulator, below
/// `folder`, simulated or not.
fn delivered(folder: &Path, script: &str, simulated: bool) -> Vec<String> {
    std::fs::write(folder.join("main.nf"), script).unwrap();
    let launch = |target: &Path| Launch {
        target: target.to_owned(),
        scratch: folder.join("scratch"),
        simulated,
        ..Launch::default()
    };
    let compiled = nextflow_compiler::compile(&launch(folder), settings(folder), folder).unwrap();
    assert!(compiled.full(), "{:?}", compiled.kept);
    let document = folder.join("compiled.wir.json");
    std::fs::write(
        &document,
        serde_json::to_string(&compiled.document).unwrap(),
    )
    .unwrap();
    let launched = folder.to_owned();
    let options = wir_runtime::nextflow::Options::new(move |_, _| settings(&launched));
    let mut adapters = wir_runtime::adapter::Adapters::default();
    wir_runtime::nextflow::register(&mut adapters, options.clone());
    let wir = wir_runtime::Settings {
        print: false,
        nextflow: options,
        adapters,
        ..wir_runtime::Settings::default()
    };
    let mut driver = wir_runtime::WirDriver::new(&launch(&document), wir).unwrap();
    let mut simulator = Simulator::new(folder.join("simulator"));
    let mut targets = Targets::default();
    let engine = Engine::new(&mut simulator).observer(&mut targets);
    engine.run("outputs", &mut driver).into_result().unwrap();
    let mut found: Vec<String> = (targets.0.iter())
        .filter_map(|target| Some(target.strip_prefix(&*folder.to_string_lossy())?.to_owned()))
        .collect();
    found.sort();
    found
}

#[test]
fn keeps_task_folders() {
    let script = "process P {\n    input:\n    val name\n    output:\n    path(\"${name}/result.txt\")\n    \
                  script:\n    \"mkdir ${name}; touch ${name}/result.txt\"\n}\n\n\
                  workflow {\n    main:\n    P(channel.of('a', 'b'))\n    publish:\n    texts = P.out\n}\n\n\
                  output {\n    texts {\n        path 'res'\n    }\n}\n";
    let directory = tempfile::tempdir().unwrap();
    let found = delivered(directory.path(), script, true);
    assert_eq!(
        found,
        ["/results/res/a/result.txt", "/results/res/b/result.txt"]
    );
}

#[test]
fn delivers_reports() {
    let script = "process P {\n    input:\n    val name\n    output:\n    path(\"${name}.txt\")\n    \
                  script:\n    \"touch ${name}.txt\"\n}\n\nworkflow {\n    P(channel.of('a'))\n}\n";
    let directory = tempfile::tempdir().unwrap();
    let reports = ["trace", "report", "timeline", "dag"]
        .map(|kind| format!("{kind}.enabled = true\n{kind}.file = 'info/{kind}.txt'\n"))
        .concat();
    std::fs::write(directory.path().join("nextflow.config"), reports).unwrap();
    let found = delivered(directory.path(), script, false);
    for kind in ["trace", "report", "timeline", "dag"] {
        let target = format!("/info/{kind}.txt");
        assert!(found.contains(&target), "{target} in {found:?}");
    }
}
