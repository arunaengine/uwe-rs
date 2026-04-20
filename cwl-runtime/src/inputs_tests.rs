//! Tests of input objects on placeholder inputs.
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
