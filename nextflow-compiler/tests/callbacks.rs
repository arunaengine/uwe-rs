//! Groovy callbacks of compiled pipelines run in the run of their script: with the inputs and
//! the run name of the wir run, and with the scripts as they are when the run starts.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::sync::{Arc, Mutex};

use nextflow_runtime::driver::Settings;
use workflow_graph::{Attempt, Executor, Launch, Outcome, Running, Simulator, execute};

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
    let mut simulator = Simulator::new(folder.join("simulator"));
    printed_by(folder, (inputs, run), &mut simulator)
}

/// Ends every task with the exit code `exit`, and keeps when the first one started.
struct Exiting {
    exit: i32,
    first: Option<u128>,
}

impl Executor for Exiting {
    fn run(&mut self, attempt: Attempt) -> Running {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
        self.first.get_or_insert(now.unwrap().as_millis());
        Running::ended(Ok(Outcome {
            id: attempt.task.id,
            exit_code: self.exit,
            ..Outcome::default()
        }))
    }
}

/// What the ops of the pipeline in `folder` printed in a run by `executor`.
fn printed_by(folder: &Path, (inputs, run): (&str, &str), executor: &mut dyn Executor) -> String {
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
    // A failed run prints too, like its `onError` handlers.
    let _ = execute("callbacks", &mut driver, executor).into_result();
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
    let mailed = printed(folder, r#"{"greeting": "mail"}"#, "mail");
    assert_eq!(mailed, "[1]\ndone\n", "the channel holds its value");
    // A handler that names `b` before `a` gets each channel by its name.
    let script = "workflow {\n    def a = channel.of(1).toList()\n    \
                  def b = channel.of(2).toList()\n    \
                  workflow.onComplete {\n        println \"${b.getVal()} ${a.getVal()}\"\n    }\n}\n";
    std::fs::write(folder.join("main.nf"), script).unwrap();
    assert_eq!(printed(folder, "{}", "both"), "[2] [1]\n");
}

#[test]
fn handlers_see_tasks() {
    let directory = tempfile::tempdir().unwrap();
    let folder = directory.path();
    let script = "process P {\n    input:\n    val x\n    script:\n    'true'\n}\n\
                  workflow {\n    workflow.onComplete {\n        \
                  println \"complete ${workflow.stats.succeededCount} ${workflow.start.getTime()}\"\n    }\n    \
                  workflow.onError { trace ->\n        \
                  println \"error ${trace.process} ${trace.exit} ${trace.status} ${workflow.exitStatus}\"\n    }\n    \
                  P(channel.of(1, 2))\n}\n";
    std::fs::write(folder.join("main.nf"), script).unwrap();
    let mut good = Exiting {
        exit: 0,
        first: None,
    };
    let text = printed_by(folder, ("{}", "good"), &mut good);
    let words: Vec<&str> = text.split_whitespace().collect();
    assert_eq!(words[..2], ["complete", "2"], "{text}");
    // The duration counts from the start of the run, before the first task.
    let start: u128 = words[2].parse().unwrap();
    assert!(start <= good.first.unwrap(), "{text}");
    let mut bad = Exiting {
        exit: 3,
        first: None,
    };
    let text = printed_by(folder, ("{}", "bad"), &mut bad);
    assert!(text.starts_with("complete 0 "), "{text}");
    assert!(
        text.ends_with("error P 3 FAILED 3\n"),
        "the trace of the task: {text}"
    );
}

#[test]
fn failed_calls_capture() {
    let directory = tempfile::tempdir().unwrap();
    let folder = directory.path();
    // The report binds before the process of its workflow fails, as nf-core's MultiQC does.
    let script = "process P {\n    input:\n    val x\n    script:\n    'true'\n}\n\
                  workflow INNER {\n    main:\n    def report = channel.of(1).toList()\n    \
                  workflow.onComplete {\n        println \"report ${report.getVal()}\"\n    }\n    \
                  P(report)\n}\nworkflow {\n    INNER()\n}\n";
    std::fs::write(folder.join("main.nf"), script).unwrap();
    let mut failing = Exiting {
        exit: 3,
        first: None,
    };
    let text = printed_by(folder, ("{}", "failed"), &mut failing);
    assert_eq!(text, "report [1]\n");
}

#[test]
fn plugins_end_runs() {
    let directory = tempfile::tempdir().unwrap();
    let folder = directory.path();
    let config = "plugins {\n    id 'nf-fl86'\n}\nparams.outdir = 'out'\n";
    std::fs::write(folder.join("nextflow.config"), config).unwrap();
    std::fs::write(
        folder.join("main.nf"),
        "workflow {\n    channel.of(1).view()\n}\n",
    )
    .unwrap();
    assert_eq!(printed(folder, "{}", "plugins"), "1\n");
    // nf-fl86 writes the metadata of the run once it ended, as in a native run.
    let written = folder.join("out/pipeline_info/workflow.json");
    assert!(written.is_file(), "the plugin acts at the end");
}
