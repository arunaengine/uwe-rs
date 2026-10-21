//! Tests of placeholder values.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_parser::model::{ArraySchema, RecordField, RecordSchema};

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
fn names_array_placeholders() {
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
fn builds_directory_placeholders() {
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
fn preserves_record_formats() {
    let field = RecordField {
        name: "reads".into(),
        value_type: Type::File,
        label: None,
        doc: Vec::new(),
        secondary_files: Vec::new(),
        streamable: None,
        format: vec![
            Expression::parse("http://edamontology.org/format_1930", false).expect("a format"),
        ],
        load_contents: None,
        load_listing: None,
        input_binding: None,
        output_binding: None,
    };
    let record = Type::Record(Box::new(RecordSchema {
        name: None,
        fields: vec![field],
        label: None,
        doc: Vec::new(),
        binding: None,
    }));
    let sample = value(&Types::default(), &record, "sample", BASE, &Hint::default());
    let reads = sample.get("reads");
    assert_eq!(
        reads.and_then(|file| file.text("location")),
        Some("file:///in/reads.fastq")
    );
    assert_eq!(
        reads.and_then(|file| file.text("format")),
        Some("http://edamontology.org/format_1930")
    );
}

#[test]
fn marks_simulated_files() {
    let file = |location: &str| {
        let mut map = Map::new();
        map.insert("class".into(), Value::string("File"));
        map.insert("location".into(), Value::string(location));
        Value::Map(map)
    };
    let prefixes = ["file:///scratch/simulator/work/1/".to_owned()];
    assert!(is_fake(
        &file("file:///scratch/simulator/work/1/out.bam"),
        &prefixes
    ));
    assert!(is_fake(
        &file("file:///no/such/folder/reads.fastq"),
        &prefixes
    ));
    let real = std::env::current_exe().expect("the test binary");
    let real = file(&cwl_parser::iri::file_url(&real));
    assert!(!is_fake(&Value::List(vec![real]), &prefixes));
}
