//! Tests of command line bindings: sort order, prefixes, arrays, records and quoting.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_parser::source::Memory;
use cwl_parser::{Loader, model::short_name};

use super::*;
use crate::value::Map;

/// The arguments the inputs of a tool give for a job, without baseCommand and arguments.
fn command_line(tool: &str, job: &str) -> Vec<String> {
    let mut memory = Memory::default();
    let url = "file:///work/tool.cwl";
    memory.insert(url, tool);
    let process = Loader::new(memory)
        .load(url)
        .expect("the tool loads")
        .process;
    let job = Value::parse(job).expect("a job");
    let types = Types::default();
    let mut positions = |_: &Expression, _: &Value| Ok(Value::Integer(0));
    let mut binder = Binder {
        types: &types,
        positions: &mut positions,
    };
    let mut bindings = Vec::new();
    for input in &process.inputs {
        let name = short_name(&input.id);
        let value = job.get(name).cloned().unwrap_or(Value::Null);
        if value.is_null() {
            continue;
        }
        bindings.extend(
            binder
                .bind_input(&input.value_type, input.binding.as_ref(), &value, name)
                .expect("binds"),
        );
    }
    sort(&mut bindings);
    bindings
        .iter()
        .flat_map(|bound| arguments(bound, &bound.value).expect("arguments"))
        .map(|arg| arg.text)
        .collect()
}

const HEAD: &str = "cwlVersion: v1.2\nclass: CommandLineTool\noutputs: []\n";

#[test]
fn sorts_by_position_then_name() {
    let tool = format!(
        "{HEAD}inputs:\n  b: {{type: int, inputBinding: {{position: 1, prefix: -b}}}}\n  \
         a: {{type: int, inputBinding: {{position: 1, prefix: -a}}}}\n  \
         c: {{type: string, inputBinding: {{}}}}\n"
    );
    assert_eq!(
        command_line(&tool, "{a: 1, b: 2, c: x}"),
        ["x", "-a", "1", "-b", "2"]
    );
}

#[test]
fn binds_arrays_with_nested_prefixes_and_separators() {
    let tool = format!(
        "{HEAD}inputs:\n  reads:\n    type: {{type: array, items: string, inputBinding: {{prefix: -Y}}}}\n    \
         inputBinding: {{position: 2, prefix: -X}}\n  \
         list:\n    type: int[]\n    inputBinding: {{position: 1, prefix: -I, itemSeparator: ','}}\n  \
         flag: {{type: boolean, inputBinding: {{prefix: --on}}}}\n  \
         off: {{type: boolean, inputBinding: {{prefix: --off}}}}\n  \
         joined: {{type: string, inputBinding: {{prefix: --x=, separate: false}}}}\n"
    );
    assert_eq!(
        command_line(
            &tool,
            "{reads: [r1, r2], list: [1, 2, 3], flag: true, off: false, joined: v}"
        ),
        ["--on", "--x=v", "-I", "1,2,3", "-X", "-Y", "r1", "-Y", "r2"]
    );
}

#[test]
fn binds_record_fields_in_their_own_order() {
    let tool = format!(
        "{HEAD}inputs:\n  r:\n    type:\n      type: record\n      fields:\n        \
         z: {{type: int, inputBinding: {{position: 1, prefix: -z}}}}\n        \
         y: {{type: int, inputBinding: {{position: 1, prefix: -y}}}}\n    \
         inputBinding: {{position: 3, prefix: -r}}\n  \
         first: {{type: string, inputBinding: {{position: 2}}}}\n"
    );
    assert_eq!(
        command_line(&tool, "{r: {z: 1, y: 2}, first: f}"),
        ["f", "-r", "-y", "2", "-z", "1"]
    );
}

#[test]
fn picks_the_union_member_that_fits() {
    let tool = format!(
        "{HEAD}inputs:\n  u:\n    type: ['null', int, {{type: array, items: string, inputBinding: {{prefix: -s}}}}]\n    \
         inputBinding: {{prefix: -u}}\n"
    );
    assert_eq!(command_line(&tool, "{u: 5}"), ["-u", "5"]);
    assert_eq!(
        command_line(&tool, "{u: [a, b]}"),
        ["-u", "-s", "a", "-s", "b"]
    );
}

#[test]
fn writes_files_by_path_and_numbers_as_python_does() {
    let tool = format!(
        "{HEAD}inputs:\n  f: {{type: File, inputBinding: {{}}}}\n  \
         x: {{type: double, inputBinding: {{position: 1}}}}\n"
    );
    let mut file = Map::new();
    file.insert("class".into(), Value::string("File"));
    file.insert("path".into(), Value::string("/in/a.txt"));
    let job = Value::Map(
        [
            ("f".to_owned(), Value::Map(file)),
            ("x".to_owned(), Value::Float(1e-7)),
        ]
        .into_iter()
        .collect(),
    );
    assert_eq!(
        command_line(&tool, &job.to_json()),
        ["/in/a.txt", "0.0000001"]
    );
}

#[test]
fn quotes_for_the_shell_as_python_does() {
    assert_eq!(shell_quote("plain-arg_1.txt"), "plain-arg_1.txt");
    assert_eq!(shell_quote(""), "''");
    assert_eq!(shell_quote("a b"), "'a b'");
    assert_eq!(shell_quote("it's"), "'it'\\''s'");
}
