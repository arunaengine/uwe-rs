//! Checks the build phase of a run: the interpreter hands factories, operators and process
//! calls to the host, runs workflow bodies itself, and keeps `.out` of each call.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::interpreter::{Error, Host, Interpreter};
use nextflow_evaluator::lower::lower_module;
use nextflow_evaluator::lowered::{Module, Process};
use nextflow_evaluator::value::{ChannelId, ChannelOut, Value};
use nextflow_parser::parse_script;

/// Records what the build asks for and answers with new channel numbers.
#[derive(Default)]
struct Network {
    calls: Vec<String>,
    channels: u32,
}

impl Network {
    fn channel(&mut self) -> Value {
        self.channels += 1;
        Value::Channel(ChannelId(self.channels))
    }
}

fn describe(value: &Value) -> String {
    match value {
        Value::Channel(id) => format!("#{}", id.0),
        Value::ChannelOut(out) => format!(
            "out{:?}",
            out.values.iter().map(describe).collect::<Vec<_>>()
        ),
        Value::Closure(_) => "{..}".into(),
        other => other.to_string(),
    }
}

impl Host for Network {
    fn print(&mut self, _: &str) {}

    fn namespace_method(
        &mut self,
        namespace: &str,
        name: &str,
        arguments: Vec<Value>,
    ) -> Result<Option<Value>, Error> {
        let arguments: Vec<String> = arguments.iter().map(describe).collect();
        let channel = self.channel();
        self.calls.push(format!(
            "{namespace}.{name}({}) = {}",
            arguments.join(", "),
            describe(&channel)
        ));
        Ok(Some(channel))
    }

    fn operator(
        &mut self,
        source: &Value,
        name: &str,
        arguments: Vec<Value>,
    ) -> Result<Option<Value>, Error> {
        let arguments: Vec<String> = arguments.iter().map(describe).collect();
        let channel = self.channel();
        self.calls.push(format!(
            "{}.{name}({}) = {}",
            describe(source),
            arguments.join(", "),
            describe(&channel)
        ));
        Ok(Some(channel))
    }

    fn call_process(
        &mut self,
        _: &Rc<Module>,
        process: &Rc<Process>,
        name: &str,
        arguments: Vec<Value>,
    ) -> Result<Value, Error> {
        let arguments: Vec<String> = arguments.iter().map(describe).collect();
        let values: Vec<Value> = process.outputs.iter().map(|_| self.channel()).collect();
        let names = process
            .outputs
            .iter()
            .map(|output| output.emit.clone())
            .collect();
        let out = Value::ChannelOut(Rc::new(ChannelOut { values, names }));
        self.calls.push(format!(
            "{name}({}) = {}",
            arguments.join(", "),
            describe(&out)
        ));
        Ok(out)
    }
}

const PROCESS: &str = "process P {\n    input:\n    val x\n    output:\n    val x, emit: out\n    script:\n    \"echo ${x}\"\n}\n";

fn build(workflows: &str) -> (Result<(), String>, Vec<String>) {
    let source = format!("{PROCESS}{workflows}");
    let parsed = parse_script(&source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let module = Rc::new(lower_module(&parsed.tree, &source).expect("lowers"));
    let mut network = Network::default();
    let mut interpreter = Interpreter::new(&mut network);
    interpreter.set_global("channel", Value::Namespace("channel".into()));
    let result = interpreter
        .run_entry(&module)
        .map(drop)
        .map_err(|error| error.to_string());
    (result, network.calls)
}

#[test]
fn wires_workflow_dataflow() {
    let (result, calls) = build(
        "workflow W {\n    take:\n    ch\n    main:\n    P(ch)\n    emit:\n    out = P.out.out\n    n = 1\n}\n\
         workflow {\n    ch = channel.of(1, 2).map { v -> v * 2 }\n    W(ch)\n    W.out.out.view()\n    W.out.n.view()\n}",
    );
    assert_eq!(result, Ok(()));
    assert_eq!(
        calls,
        [
            "channel.of(1, 2) = #1",
            "#1.map({..}) = #2",
            "W:P(#2) = out[\"#3\"]",
            "channel.value(1) = #4",
            "#3.view() = #5",
            "#4.view() = #6",
        ]
    );
}

#[test]
fn pipes() {
    let (result, calls) = build("workflow {\n    channel.of(1) | map { v -> v } | P | view\n}");
    assert_eq!(result, Ok(()));
    assert_eq!(
        calls,
        [
            "channel.of(1) = #1",
            "#1.map({..}) = #2",
            "P(#2) = out[\"#3\"]",
            "out[\"#3\"].view() = #4",
        ]
    );
}

#[test]
fn requires_output_call() {
    let (result, _) = build("workflow {\n    P.out.view()\n}");
    let message = "Access to 'P.out' is undefined since the process 'P' has not been invoked before accessing the output attribute";
    assert_eq!(result, Err(format!("IllegalStateException: {message}")));
}
