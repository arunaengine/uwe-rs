//! Tests of placeholder values, and of input objects on placeholder inputs.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::Arc;

use cwl_parser::Loader;
use cwl_parser::model::ArraySchema;
use cwl_parser::source::Memory;

use super::*;
use crate::expression::Evaluator;
use crate::files::Access;
use crate::inputs::{Setting, prepare};

const BASE: &str = "file:///in/";

fn process(text: &str) -> Arc<Process> {
    let mut memory = Memory::default();
    memory.insert("file:///work/tool.cwl", text);
    let document = Loader::new(memory).load("file:///work/tool.cwl");
    document.unwrap_or_else(|error| panic!("{error}")).process
}

fn locations(value: &Value) -> Vec<&str> {
    let items = value.as_list().unwrap_or_default();
    items
        .iter()
        .filter_map(|item| item.text("location"))
        .collect()
}

#[test]
fn arrays_get_two_items_with_their_own_names() {
    let array = Type::Array(Box::new(ArraySchema {
        items: Type::File,
        name: None,
        label: None,
        doc: Vec::new(),
        binding: None,
    }));
    let hint = Hint {
        extension: ".fastq".into(),
        format: None,
    };
    let reads = value(&Types::default(), &array, "reads", BASE, &hint);
    assert_eq!(
        locations(&reads),
        ["file:///in/reads.fastq", "file:///in/reads_2.fastq"]
    );
}

#[test]
fn directories_hold_a_folder_with_a_file_and_a_file() {
    let genome = value(
        &Types::default(),
        &Type::Directory,
        "genome",
        BASE,
        &Hint::default(),
    );
    let listing = genome.get("listing").cloned().unwrap_or(Value::Null);
    assert_eq!(
        locations(&listing),
        ["file:///in/genome/genome_1", "file:///in/genome/genome.txt"]
    );
    let inner = listing.as_list().and_then(|items| items.first());
    let inner_listing = inner.and_then(|folder| folder.get("listing"));
    assert_eq!(
        inner_listing.map(locations),
        Some(vec!["file:///in/genome/genome_1/genome_1.txt"])
    );
}

#[test]
fn optional_inputs_of_the_top_process_get_placeholders_when_asked() {
    let tool = process(
        "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: cat\noutputs: []\n\
         inputs:\n  extra: File?\n",
    );
    let (types, evaluator) = (Types::default(), Evaluator::default());
    let setting = Setting {
        types: &types,
        evaluator: &evaluator,
        access: Access {
            assume_present: true,
        },
        load_listing: None,
        discover: true,
        placeholders: Some(BASE),
        toplevel: true,
        fill_optional: true,
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
