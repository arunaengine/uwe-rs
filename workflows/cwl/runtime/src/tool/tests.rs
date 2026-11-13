//! Tests of the parts of command line tools that need no whole process.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::workdir::dirent_entries;
use super::*;
use cwl_parser::model::Dirent;

#[test]
fn stages_dirent_lists() {
    let dirent = |entry: &str| Dirent {
        entryname: None,
        entry: Expression::parse(entry, false).expect("an expression"),
        writable: false,
    };
    let inputs = Value::parse(
        "{empty: {class: Directory, listing: []}, \
          full: {class: Directory, listing: [{class: File, location: 'file:///a'}]}}",
    )
    .expect("YAML");
    let vars = Variables {
        inputs: &inputs,
        this: &Value::Null,
        runtime: &Value::Null,
    };
    let evaluator = Evaluator::default();
    let empty = dirent_entries(&dirent("$(inputs.empty.listing)"), &evaluator, vars);
    assert_eq!(empty, Ok(Vec::new()));
    let full = dirent_entries(&dirent("$(inputs.full.listing)"), &evaluator, vars);
    assert_eq!(full.map(|entries| entries.len()), Ok(1));
}
