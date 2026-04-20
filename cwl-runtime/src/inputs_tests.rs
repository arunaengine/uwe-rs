//! Tests of input objects: placeholders of the top process, and format checks.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::Arc;

use cwl_parser::Loader;
use cwl_parser::source::Memory;

use super::*;
use crate::expression::Evaluator;

const BASE: &str = "file:///in/";

fn process(text: &str) -> Arc<Process> {
    let mut memory = Memory::default();
    memory.insert("file:///work/tool.cwl", text);
    let document = Loader::new(memory).load("file:///work/tool.cwl");
    document.unwrap_or_else(|error| panic!("{error}")).process
}

/// The setting of the process a run on placeholder inputs starts with.
fn setting<'a>(types: &'a Types, evaluator: &'a Evaluator) -> Setting<'a> {
    Setting {
        types,
        evaluator,
        access: Access {
            assume_present: true,
        },
        load_listing: None,
        discover: true,
        placeholders: Some(BASE),
        toplevel: true,
        fill_optional: false,
        ontologies: false,
    }
}

#[test]
fn optional_inputs_of_the_top_process_get_placeholders_when_asked() {
    let tool = process(
        "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: cat\noutputs: []\n\
         inputs:\n  extra: File?\n",
    );
    let (types, evaluator) = (Types::default(), Evaluator::default());
    let setting = Setting {
        fill_optional: true,
        ..setting(&types, &evaluator)
    };
    let job = prepare(&tool, Map::new(), &setting).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        job.get("extra").and_then(|file| file.text("location")),
        Some("file:///in/extra.txt")
    );
    let plain = Setting {
        fill_optional: false,
        ..setting
    };
    let job = prepare(&tool, Map::new(), &plain).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(job.get("extra"), Some(&Value::Null));
}

#[test]
fn broken_defaults_of_the_top_process_give_way_to_placeholders() {
    let tool = process(
        "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: cat\noutputs: []\n\
         inputs:\n  given:\n    type: File\n    default: {class: File}\n",
    );
    let (types, evaluator) = (Types::default(), Evaluator::default());
    let setting = setting(&types, &evaluator);
    let job = prepare(&tool, Map::new(), &setting).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        job.get("given").and_then(|file| file.text("location")),
        Some("file:///in/given.txt")
    );
    // Inside a workflow such a default is an error, as in cwltool.
    let inner = Setting {
        toplevel: false,
        ..setting
    };
    let job = prepare(&tool, Map::new(), &inner);
    assert!(job.is_err_and(|error| error.contains("File object")));
}

const FORMATS: &str = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: 'true'\n\
    outputs: []\ninputs:\n  regular: {type: File, format: 'http://example.com/format1'}\n  \
    record:\n    type:\n      type: record\n      fields:\n        \
    f2: {type: 'File[]', format: 'http://example.com/format2'}\n";

/// The input object of the format tool with these two formats, which `None` leaves out.
fn formats(regular: Option<&str>, item: &str) -> Map {
    let mut file = "{class: File, location: 'file:///d'".to_owned();
    if let Some(format) = regular {
        file.push_str(&format!(", format: '{format}'"));
    }
    file.push('}');
    let job = format!(
        "{{regular: {file}, record: {{f2: [{{class: File, location: 'file:///b', \
         format: '{item}'}}]}}}}"
    );
    match Value::parse(&job) {
        Ok(Value::Map(map)) => map,
        other => panic!("{other:?}"),
    }
}

#[test]
fn checks_file_formats_as_cwltool_does_without_an_ontology() {
    let tool = process(FORMATS);
    let (types, evaluator) = (Types::default(), Evaluator::default());
    let setting = setting(&types, &evaluator);
    let one = "http://example.com/format1";
    let two = "http://example.com/format2";
    let other = "http://example.com/formatZ";
    assert!(prepare(&tool, formats(Some(one), two), &setting).is_ok());
    let wrong = prepare(&tool, formats(Some(other), two), &setting);
    assert!(wrong.is_err_and(|error| error.contains("formatZ")));
    let wrong_item = prepare(&tool, formats(Some(one), other), &setting);
    assert!(wrong_item.is_err_and(|error| error.contains("formatZ")));
    let missing = prepare(&tool, formats(None, two), &setting);
    assert!(missing.is_err_and(|error| error.contains("no format")));
    // A placeholder file without a format takes the declared one.
    let mut job = formats(None, two);
    if let Some(Value::Map(file)) = job.get_mut("regular") {
        file.insert("location".into(), Value::string("file:///in/regular.txt"));
    }
    let job = prepare(&tool, job, &setting).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        job.get("regular").and_then(|file| file.text("format")),
        Some(one)
    );
    // With ontologies named, another format may be a subclass of a declared one.
    let named = Setting {
        ontologies: true,
        ..setting
    };
    assert!(prepare(&tool, formats(Some(other), two), &named).is_ok());
}
