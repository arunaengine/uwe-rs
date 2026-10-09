//! Checks persistent process output reuse and completion delivery.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::interpreter::Host;
use nextflow_evaluator::lower::lower_module;
use nextflow_evaluator::value::PathValue;
use nextflow_parser::parse_script;
use nextflow_runtime::{Options, Run, TaskOutcome};
use std::rc::Rc;

#[derive(Default)]
struct Output(String);
impl Host for Output {
    fn print(&mut self, text: &str) {
        self.0.push_str(text);
    }
}

#[test]
fn reuses_stored_outputs() {
    let root = tempfile::tempdir().unwrap();
    let store = root.path().join("stored");
    std::fs::create_dir(&store).unwrap();
    std::fs::write(store.join("out.txt"), "stored").unwrap();
    let source = format!(
        "process P {{\n storeDir '{}'\n output:\n path 'out.txt'\n path 'optional.txt', optional: true\n script:\n 'exit 9'\n}}\nworkflow {{ P(); P.out[0].view {{ it.text }} }}",
        store.display()
    );
    let parsed = parse_script(&source);
    let module = Rc::new(lower_module(&parsed.tree, &source).unwrap());
    let mut output = Output::default();
    let (mut run, requests) = Run::start(&module, &Options::default(), &mut output).unwrap();
    assert!(requests.is_empty());
    run.finish(&mut output).unwrap();
    assert_eq!(output.0, "stored\n");
}

#[test]
fn stores_completed_outputs() {
    let root = tempfile::tempdir().unwrap();
    let store = root.path().join("stored");
    std::fs::create_dir(&store).unwrap();
    std::fs::write(store.join("optional.txt"), "optional").unwrap();
    let source = format!(
        "process P {{\n storeDir '{}'\n output:\n path 'out.txt'\n path 'optional.txt', optional: true\n script:\n 'true'\n}}\nworkflow {{ P(); P.out[0].view {{ it.text }} }}",
        store.display()
    );
    let parsed = parse_script(&source);
    let module = Rc::new(lower_module(&parsed.tree, &source).unwrap());
    let mut output = Output::default();
    let (mut run, mut requests) = Run::start(&module, &Options::default(), &mut output).unwrap();
    assert_eq!(requests.len(), 1);
    let request = requests.pop().unwrap();
    let original = root.path().join("original");
    std::fs::write(&original, "computed").unwrap();
    let requests = run
        .complete(
            TaskOutcome {
                id: request.id,
                files: vec![(
                    "out.txt".into(),
                    PathValue::parse(&original.to_string_lossy(), "/"),
                )],
                ..TaskOutcome::default()
            },
            &mut output,
        )
        .unwrap();
    assert!(requests.is_empty());
    run.finish(&mut output).unwrap();
    assert_eq!(
        std::fs::read_to_string(store.join("out.txt")).unwrap(),
        "computed"
    );
    assert!(original.exists());
    assert_eq!(output.0, "computed\n");
}

#[test]
fn optional_outputs_skip() {
    let root = tempfile::tempdir().unwrap();
    let source = format!(
        "process P {{\n storeDir '{}'\n output:\n path '*.txt', optional: true\n script:\n 'exit 9'\n}}\nworkflow {{ P() }}",
        root.path().display()
    );
    let parsed = parse_script(&source);
    let module = Rc::new(lower_module(&parsed.tree, &source).unwrap());
    let (_, requests) = Run::start(&module, &Options::default(), &mut Output::default()).unwrap();
    assert!(requests.is_empty());
}

#[test]
fn refuses_store_after() {
    let source = "process P {\n storeDir 'stored'\n afterScript 'true'\n output:\n path 'out.txt'\n script:\n 'true'\n}\nworkflow { P() }";
    let parsed = parse_script(source);
    let module = Rc::new(lower_module(&parsed.tree, source).unwrap());
    let error = Run::start(&module, &Options::default(), &mut Output::default()).unwrap_err();
    assert!(error.to_string().contains("output transfer before the hook"));
}
