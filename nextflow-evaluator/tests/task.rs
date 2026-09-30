//! Renders the nf-core GUNZIP module for one task and compares the task script with the
//! `.command.sh` that Nextflow 26.04.6 wrote (`tests/fixtures/oracle/gunzip/record.sh`).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::interpreter::{
    Host, Interpreter, RenderedPart, RenderedTask, TaskDirective,
};
use nextflow_evaluator::lower::lower_module;
use nextflow_evaluator::value::{PathValue, Value};
use nextflow_parser::parse_script;

const GUNZIP: &str = include_str!("fixtures/gunzip.nf");
const SCRIPT: &str = include_str!("fixtures/oracle/gunzip/script.command.sh");
const STUB: &str = include_str!("fixtures/oracle/gunzip/stub.command.sh");

struct Launch;

impl Host for Launch {
    fn print(&mut self, _: &str) {}

    fn launch_dir(&self) -> String {
        "/launch".into()
    }
}

/// The oracle script up to the block that captures `eval` outputs, which iunctus writes itself.
fn before_capture(command: &str) -> &str {
    let end = command
        .find("\n# capture process environment")
        .expect("capture block");
    &command[..end]
}

fn render(stub: bool) -> (Vec<TaskDirective>, RenderedTask) {
    let parsed = parse_script(GUNZIP);
    let module = Rc::new(lower_module(&parsed.tree, GUNZIP).expect("lowers"));
    let process = module.processes[0].clone();
    let mut host = Launch;
    let mut interpreter = Interpreter::new(&mut host);
    interpreter.set_global(
        "workflow",
        Value::map([(Value::string("containerEngine"), Value::Null)]),
    );
    *module.directory.borrow_mut() = Some("/modules/gunzip".into());
    let meta = Value::map([(Value::string("id"), Value::string("S1"))]);
    let archive = Value::Path(Rc::new(PathValue::parse("/data/sample.fastq.gz", "/")));
    let input = Value::list(vec![meta, archive]);
    let mut task = interpreter
        .bind_task(&module, &process, &[input])
        .expect("binds");
    let task_object = Value::map([
        (Value::string("process"), Value::string("GUNZIP")),
        (Value::string("attempt"), Value::integer(1)),
        (Value::string("ext"), Value::map([])),
    ]);
    let directives = interpreter
        .directives(&mut task, &process, &task_object)
        .expect("evaluates");
    let rendered = interpreter
        .render(&mut task, &process, &task_object, stub)
        .expect("renders");
    (directives, rendered)
}

#[test]
fn script_matches_nextflow() {
    let (_, rendered) = render(false);
    assert!(rendered.run);
    assert_eq!(rendered.script, before_capture(SCRIPT));
}

#[test]
fn stub_matches_nextflow() {
    let (_, rendered) = render(true);
    assert_eq!(rendered.script, before_capture(STUB));
}

#[test]
fn directives_and_outputs() {
    let (directives, rendered) = render(false);
    let text: Vec<String> = directives
        .iter()
        .map(|directive| format!("{} {}", directive.name, directive.arguments[0]))
        .collect();
    assert_eq!(text[0], "tag sample.fastq.gz");
    assert_eq!(text[1], "label process_single");
    assert_eq!(text[2], "conda /modules/gunzip/environment.yml");
    assert_eq!(
        text[3],
        "container community.wave.seqera.io/library/coreutils_grep_gzip_lbzip2_pruned:838ba80435a629f8"
    );
    let [files, versions] = rendered.outputs.as_slice() else {
        panic!("two outputs")
    };
    assert_eq!(files.emit.as_deref(), Some("gunzip"));
    assert!(
        matches!(&files.parts[1], RenderedPart::Path { patterns, .. } if patterns == &["sample.fastq"])
    );
    assert!(
        matches!(&versions.parts[0], RenderedPart::Value(Value::String(process)) if &**process == "GUNZIP")
    );
    let command = r#"gunzip --version 2>&1 | head -1 | sed "s/^.*(gzip) //; s/ Copyright.*//""#;
    assert!(matches!(&versions.parts[2], RenderedPart::Eval(eval) if eval == command));
    assert_eq!(versions.topic.as_deref(), Some("versions"));
}

#[test]
fn binds_round_inputs() {
    for declaration in [
        "env('SAMPLE')\n    stdin",
        "each env('SAMPLE')\n    stdin",
        "tuple env('SAMPLE'), stdin",
    ] {
        let source =
            format!("process INPUTS {{\n    input:\n    {declaration}\n    script:\n    'cat'\n}}");
        let parsed = parse_script(&source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let module = Rc::new(lower_module(&parsed.tree, &source).expect("lowers"));
        let process = &module.processes[0];
        let mut host = Launch;
        let mut interpreter = Interpreter::new(&mut host);
        for sample in ["A", "B"] {
            let values = vec![Value::string(sample), Value::string("/literal/input\n")];
            let inputs = if declaration.starts_with("tuple") {
                vec![Value::list(values)]
            } else {
                values
            };
            let task = interpreter
                .bind_task(&module, process, &inputs)
                .expect("binds");
            assert_eq!(task.input_environment.len(), 1);
            assert_eq!(&*task.input_environment[0].0, "SAMPLE");
            assert_eq!(task.input_environment[0].1.to_string(), sample);
            assert_eq!(task.stdin.unwrap().to_string(), "/literal/input\n");
            assert!(task.staged.is_empty());
        }
    }
}
