//! Groovy callbacks of compiled pipelines run in the run of their script: with the inputs and
//! the run name of the wir run, and with the scripts as they are when the run starts.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::sync::{Arc, Mutex};

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

fn settings(folder: &Path, run: &str) -> Settings {
    Settings {
        host: Box::new(Quiet(folder.to_string_lossy().into_owned())),
        run_name: run.into(),
        ..Settings::default()
    }
}

/// Compiles the pipeline in `folder` and runs it simulated with these inputs and this run
/// name; what its ops printed.
fn printed(folder: &Path, inputs: &str, run: &str) -> String {
    let launch = |target: &Path| Launch {
        target: target.to_owned(),
        scratch: folder.join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let compiled = nextflow_compiler::compile(&launch(folder), settings(folder, run), folder);
    let document = folder.join("compiled.wir.json");
    let text = serde_json::to_string(&compiled.unwrap().document).unwrap();
    std::fs::write(&document, text).unwrap();
    std::fs::write(folder.join("inputs.json"), inputs).unwrap();
    let text = Arc::new(Mutex::new(String::new()));
    let sink = text.clone();
    let (launched, name) = (folder.to_owned(), run.to_owned());
    let options = wir_runtime::nextflow::Options::new(move |_, _| settings(&launched, &name));
    let mut adapters = wir_runtime::adapter::Adapters::default();
    wir_runtime::nextflow::register(&mut adapters, options.clone());
    let wir = wir_runtime::Settings {
        print: false,
        print_sink: wir_runtime::Printer::new(move |line| sink.lock().unwrap().push_str(line)),
        nextflow: options,
        adapters,
        ..wir_runtime::Settings::default()
    };
    let run = Launch {
        inputs: Some(folder.join("inputs.json")),
        ..launch(&document)
    };
    let mut driver = wir_runtime::WirDriver::new(&run, wir).unwrap();
    let mut simulator = Simulator::new(folder.join("simulator"));
    // A failed run prints too, like its `onError` handlers.
    let _ = execute("callbacks", &mut driver, &mut simulator).into_result();
    drop(driver);
    text.lock().unwrap().clone()
}

const SCRIPT: &str = "def tag() { 'one' }\nworkflow {\n    \
                      channel.of(1).map { \"${params.greeting} ${workflow.runName} ${tag()}\" }.view()\n}\n";

#[test]
fn callbacks_see_runs() {
    let directory = tempfile::tempdir().unwrap();
    let folder = directory.path();
    std::fs::write(folder.join("main.nf"), SCRIPT).unwrap();
    std::fs::write(folder.join("nextflow.config"), "params.greeting = 'hi'\n").unwrap();
    let given = printed(folder, r#"{"greeting": "hey"}"#, "first");
    assert_eq!(
        given, "hey first one\n",
        "the inputs and the name of the run"
    );
    let again = printed(folder, r#"{"greeting": "hey"}"#, "second");
    assert_eq!(
        again, "hey second one\n",
        "a new run has its own interpreter"
    );
    std::fs::write(folder.join("main.nf"), SCRIPT.replace("'one'", "'two'")).unwrap();
    let changed = printed(folder, r#"{"greeting": "hey"}"#, "third");
    assert_eq!(
        changed, "hey third two\n",
        "a new run loads the scripts again"
    );
    let stray = Path::new(env!("CARGO_MANIFEST_DIR")).join("callbacks");
    assert!(
        !stray.exists(),
        "the callbacks keep nothing in the working folder"
    );
}

#[test]
fn runs_workflow_handlers() {
    let handlers = "    workflow.onComplete { println \"done ${workflow.success} ${workflow.exitStatus}\" }\n    \
                    workflow.onError { println \"failed: ${workflow.errorMessage}\" }\n";
    let directory = tempfile::tempdir().unwrap();
    let folder = directory.path();
    std::fs::write(folder.join("nextflow.config"), "params.greeting = 'hi'\n").unwrap();
    let good = format!("workflow {{\n{handlers}    channel.of(1).view()\n}}\n");
    std::fs::write(folder.join("main.nf"), good).unwrap();
    assert_eq!(printed(folder, "{}", "good"), "1\ndone true 0\n");
    let bad = format!(
        "workflow {{\n{handlers}    channel.of(1).map {{ throw new Exception('boom') }}.view()\n}}\n"
    );
    std::fs::write(folder.join("main.nf"), bad).unwrap();
    let text = printed(folder, "{}", "bad");
    assert!(text.starts_with("done false 1\nfailed: "), "{text}");
    assert!(text.contains("boom"), "{text}");
}

#[test]
fn handlers_capture_channels() {
    let directory = tempfile::tempdir().unwrap();
    let folder = directory.path();
    std::fs::write(folder.join("nextflow.config"), "params.greeting = 'hi'\n").unwrap();
    let script = "workflow {\n    def reports = channel.of(1).toList()\n    \
                  workflow.onComplete {\n        if (params.greeting == 'mail') {\n            \
                  println reports.getVal()\n        }\n        println 'done'\n    }\n}\n";
    std::fs::write(folder.join("main.nf"), script).unwrap();
    assert_eq!(
        printed(folder, "{}", "channels"),
        "done\n",
        "a handler that captures a channel"
    );
}
