//! Compiled callbacks see the variables of their script as the native run set them, and an
//! error in one is shown at its closure in the script.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use nextflow_runtime::driver::Settings;
use workflow_graph::{Launch, Simulator, execute};

/// A host in the folder of the test that prints nothing.
struct Quiet(String);

/// The value of `API_TOKEN` in the environment of the test host.
const TOKEN: &str = "token-0123456789";

impl nextflow_evaluator::interpreter::Host for Quiet {
    fn print(&mut self, _: &str) {}

    fn environment(&self, name: &str) -> Option<String> {
        (name == "API_TOKEN").then(|| TOKEN.to_owned())
    }

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

/// Compiles `script` and runs it simulated, with the error of the run, if any.
fn run(script: &str) -> Result<(), String> {
    let directory = tempfile::tempdir().unwrap();
    let folder = directory.path();
    std::fs::write(folder.join("main.nf"), script).unwrap();
    let launch = |target: &Path| Launch {
        target: target.to_owned(),
        scratch: folder.join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let compiled = nextflow_compiler::compile(&launch(folder), settings(folder), folder)?;
    let document = folder.join("compiled.wir.json");
    let text = serde_json::to_string(&compiled.document).unwrap();
    std::fs::write(&document, text).unwrap();
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
    let mut driver =
        wir_runtime::WirDriver::new(&launch(&document), wir).map_err(|error| error.message)?;
    let mut simulator = Simulator::new(folder.join("simulator"));
    let execution = execute("scripts", &mut driver, &mut simulator);
    execution
        .into_result()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// As nf-core viralrecon, the cases of a `branch` write into a map of the script.
#[test]
fn reads_script_variables() {
    let script = "def seen = [:]\nworkflow {\n    channel.of(1, 2).branch { n ->\n        \
                  small: n < 2\n            seen[n] = 'small'\n            return n\n        \
                  large: true\n            seen[n] = 'large'\n            return n\n    \
                  }.set { parts }\n    parts.large.view()\n}\n";
    run(script).unwrap();
}

/// An error in a compiled callback names the line of its closure in the script.
#[test]
fn locates_callback_errors() {
    let script = "def missing = null\nworkflow {\n    channel.of(1, 2).branch { n ->\n        \
                  small: n < 2\n            missing.put(n, 1)\n            return n\n        \
                  large: true\n            return n\n    }.set { parts }\n    \
                  parts.small.view()\n}\n";
    let error = run(script).unwrap_err();
    assert!(error.contains("main.nf:5:"), "{error}");
}

/// A value of the environment that a callback reads, like a token, stays out of the document.
#[test]
fn keeps_environment_values() {
    let directory = tempfile::tempdir().unwrap();
    let folder = directory.path();
    let script = "def token = System.getenv('API_TOKEN')\nworkflow {\n    \
                  channel.of(1).map { n -> token.size() + n }.view()\n}\n";
    std::fs::write(folder.join("main.nf"), script).unwrap();
    let launch = Launch {
        target: folder.to_owned(),
        scratch: folder.join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let compiled = nextflow_compiler::compile(&launch, settings(folder), folder).unwrap();
    let text = serde_json::to_string(&compiled.document).unwrap();
    assert!(!text.contains(TOKEN));
}
