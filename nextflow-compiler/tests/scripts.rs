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
        let value = match name {
            "API_TOKEN" => TOKEN,
            "SHORT_TOKEN" => "tok",
            "QUOTED_TOKEN" => "a\"b\\c",
            "LABEL" => "label-value",
            _ => return None,
        };
        Some(value.to_owned())
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

/// Compiles `script` and runs it simulated: the scripts of its tasks, or the error of the run.
fn run(script: &str) -> Result<Vec<String>, String> {
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
    let graph = execution.into_result().map_err(|error| error.to_string())?;
    let scripts = (graph.nodes.iter()).flat_map(|node| &node.task.inputs);
    let scripts = scripts.filter(|input| input.path == ".command.sh");
    Ok(scripts.filter_map(|input| input.content.clone()).collect())
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

/// The callbacks of a run share the variables of their script, as native code does: a map
/// that one item fills is there for the next.
#[test]
fn shares_script_variables() {
    let script = "def seen = [:]\nprocess SAY {\n    input:\n    val n\n    script:\n    \
                  \"echo $n\"\n}\nworkflow {\n    \
                  SAY(channel.of('a', 'b').map { x -> seen[x] = true; seen.size() })\n}\n";
    let mut said: Vec<String> = (run(script).unwrap().iter())
        .filter_map(|script| script.lines().find(|line| line.starts_with("echo")))
        .map(str::to_owned)
        .collect();
    said.sort();
    assert_eq!(said, ["echo 1", "echo 2"]);
}

/// Compiles `script` in a folder of its own.
fn compiled(script: &str) -> nextflow_compiler::Compilation {
    let directory = tempfile::tempdir().unwrap();
    let folder = directory.path();
    std::fs::write(folder.join("main.nf"), script).unwrap();
    let launch = Launch {
        target: folder.to_owned(),
        scratch: folder.join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    nextflow_compiler::compile(&launch, settings(folder), folder).unwrap()
}

/// A short value of the environment, or one with quotes and backslashes, stays out of the
/// document too: the workflow stays native.
#[test]
fn keeps_every_value() {
    for name in ["SHORT_TOKEN", "QUOTED_TOKEN"] {
        let script = format!(
            "def token = System.getenv('{name}')\nworkflow {{\n    \
             channel.of(1).map {{ n -> token.size() + n }}.view()\n}}\n"
        );
        assert!(!compiled(&script).full(), "{name}");
    }
}

/// What one compile read from the environment does not count for the next one, whose own
/// text may equal it.
#[test]
fn notes_one_compile() {
    let reads = "def label = System.getenv('LABEL')\nworkflow {\n    \
                 channel.of(1).map { n -> label + n }.view()\n}\n";
    assert!(!compiled(reads).full());
    let writes = "def label = 'label-value'\nworkflow {\n    \
                  channel.of(1).map { n -> label + n }.view()\n}\n";
    assert!(compiled(writes).full());
}
