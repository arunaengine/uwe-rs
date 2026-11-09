//! The task adapter of a compiled run sees the params of the native run: the given inputs
//! over the params that the document was compiled with.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use nextflow_runtime::driver::Settings;
use workflow_graph::{Launch, Simulator, execute};

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

#[test]
fn sees_compiled_params() {
    let directory = tempfile::tempdir().unwrap();
    let folder = directory.path();
    let script = "process SAY {\n    output:\n    path 'said.txt'\n    script:\n    \
                  \"echo ${params.who} > said.txt\"\n}\nworkflow {\n    SAY()\n}\n";
    std::fs::write(folder.join("main.nf"), script).unwrap();
    let launch = |target: &Path| Launch {
        target: target.to_owned(),
        scratch: folder.join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    // `--who x` is given only when the pipeline is compiled.
    let compiling = Launch {
        params: vec![("who".into(), "x".into())],
        ..launch(folder)
    };
    let compiled = nextflow_compiler::compile(&compiling, settings(folder), folder).unwrap();
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
    let graph = execute("params", &mut driver, &mut simulator)
        .into_result()
        .unwrap();
    let script = (graph.nodes[0].task.inputs.iter())
        .find(|input| input.path == ".command.sh")
        .and_then(|input| input.content.clone())
        .unwrap_or_default();
    assert!(script.contains("echo x > said.txt"), "{script}");
}
