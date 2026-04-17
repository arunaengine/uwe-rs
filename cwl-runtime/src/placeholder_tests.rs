//! Tests of placeholder values.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_parser::model::ArraySchema;

use super::*;

const BASE: &str = "file:///in/";

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
