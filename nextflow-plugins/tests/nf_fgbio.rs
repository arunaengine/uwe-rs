//! nf-fgbio's read structures against nf-fgbio 1.0.1 with Nextflow 26.04.6.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_config::Config;
use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::value::Value;
use nextflow_plugins::{Context, call, method};

/// Reads the sample sheets of the fixtures, relative to their folder.
struct Fixtures;

impl Host for Fixtures {
    fn print(&mut self, _: &str) {}

    fn launch_dir(&self) -> String {
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/nf_fgbio").into()
    }
}

/// The text of a result, or the exception of the failure.
fn shown(result: Result<Value, Error>) -> String {
    match result {
        Ok(value) => value.to_string(),
        Err(Error::Thrown { exception, .. }) => exception.to_string(),
        Err(other) => panic!("{other}"),
    }
}

fn function(name: &str, arguments: Vec<Value>) -> Result<Value, Error> {
    let config = Config::default();
    let mut host = Fixtures;
    let mut context = Context {
        host: &mut host,
        config: &config,
        params: Value::map([]),
        project_dir: "/",
        cli_params: &[],
        workflow: Value::Null,
    };
    call("nf-fgbio", name, arguments, &mut context)
}

fn invoke(object: &Value, name: &str, arguments: Vec<Value>) -> Result<Value, Error> {
    let config = Config::default();
    let mut host = Fixtures;
    let mut context = Context {
        host: &mut host,
        config: &config,
        params: Value::map([]),
        project_dir: "/",
        cli_params: &[],
        workflow: Value::Null,
    };
    let found = method(object, name, arguments, &mut context)?;
    Ok(found.unwrap_or_else(|| panic!("no method {name}")))
}

/// The text of a method's result with its class, or the exception.
fn member(object: &Value, name: &str, arguments: Vec<Value>) -> String {
    match invoke(object, name, arguments) {
        Ok(value) => format!("{value} ({})", value.java_class_name()),
        failure => shown(failure),
    }
}

fn read_structure(text: &str) -> String {
    shown(function("readStructure", vec![Value::string(text)]))
}

#[test]
fn parses_read_structures() {
    let cases = [
        ("3M2S+T", "3M2S+T"),
        (" 8m 1t ", "8M1T"),
        ("1T 2T", "1T2T"),
        (
            "",
            "IllegalArgumentException: requirement failed: Can't create a read structure with \
             zero segments.",
        ),
        (
            "10",
            "IllegalArgumentException: Read structure with invalid segment: [10]",
        ),
        (
            "T",
            "IllegalArgumentException: Read structure missing length information: [T]",
        ),
        (
            "10X",
            "IllegalArgumentException: Read structure segment had unknown type: [10X]",
        ),
        (
            "8M+T10B",
            "IllegalArgumentException: requirement failed: Variable length (+) can only be used \
             in the last segment: 8M+T10B",
        ),
        (
            "0T",
            "IllegalArgumentException: Read structure contained zero length segments: 0T",
        ),
        (
            "10B20C+T",
            "IllegalArgumentException: Read structure segment had unknown type: 10B[20C]+T",
        ),
        (
            "8M0S",
            "IllegalArgumentException: Read structure contained zero length segments: 8M0S",
        ),
        (
            "abc",
            "IllegalArgumentException: Read structure missing length information: [A]BC",
        ),
        (
            "+",
            "IllegalArgumentException: Read structure with invalid segment: [+]",
        ),
        (
            "++T",
            "IllegalArgumentException: Read structure segment had unknown type: [++]T",
        ),
        (
            "1.5T",
            "IllegalArgumentException: Read structure segment had unknown type: [1.]5T",
        ),
    ];
    for (text, expected) in cases {
        assert_eq!(read_structure(text), expected, "{text:?}");
    }
}

#[test]
fn answers_structure_methods() {
    let structure =
        function("readStructure", vec![Value::string(" 4m3s12B+t ")]).expect("a structure");
    let integer = |n: i32| vec![Value::integer(n)];
    let cases = [
        ("toString", vec![], "4M3S12B+T (java.lang.String)"),
        (
            "segments",
            vec![],
            "Vector(4M, 3S, 12B, +T) (scala.collection.immutable.Vector1)",
        ),
        ("length", vec![], "4 (java.lang.Integer)"),
        ("size", vec![], "4 (java.lang.Integer)"),
        ("hasFixedLength", vec![], "false (java.lang.Boolean)"),
        (
            "fixedLength",
            vec![],
            "IllegalArgumentException: requirement failed: fixedLength called on variable \
             length segment: 4M3S12B+T",
        ),
        (
            "apply",
            integer(0),
            "4M (com.fulcrumgenomics.util.ReadSegment)",
        ),
        (
            "templateSegments",
            vec![],
            "Vector(+T) (scala.collection.immutable.Vector1)",
        ),
        (
            "sampleBarcodeSegments",
            vec![],
            "Vector(12B) (scala.collection.immutable.Vector1)",
        ),
        (
            "molecularBarcodeSegments",
            vec![],
            "Vector(4M) (scala.collection.immutable.Vector1)",
        ),
        (
            "skipSegments",
            vec![],
            "Vector(3S) (scala.collection.immutable.Vector1)",
        ),
        (
            "withVariableLastSegment",
            vec![],
            "4M3S12B+T (com.fulcrumgenomics.util.ReadStructure)",
        ),
        (
            "extract",
            vec![Value::string("AAAACCCGGGGGGGGGGGGTTTTT")],
            "Vector(SubReadWithoutQuals(AAAA,4M), SubReadWithoutQuals(CCC,3S), \
             SubReadWithoutQuals(GGGGGGGGGGGG,12B), SubReadWithoutQuals(TTTTT,5T)) \
             (scala.collection.immutable.Vector1)",
        ),
        (
            "extract",
            vec![Value::string("AAAA")],
            "IllegalArgumentException: requirement failed: Read ends before end of segment: 3S",
        ),
        (
            "extract",
            vec![
                Value::string("AAAACCCGGGGGGGGGGGGTTTTT"),
                Value::string("IIIIIIIIIIIIIIIIIIIIIIII"),
            ],
            "Vector(SubReadWithQuals(AAAA,IIII,4M), SubReadWithQuals(CCC,III,3S), \
             SubReadWithQuals(GGGGGGGGGGGG,IIIIIIIIIIII,12B), SubReadWithQuals(TTTTT,IIIII,5T)) \
             (scala.collection.immutable.Vector1)",
        ),
    ];
    for (name, arguments, expected) in cases {
        assert_eq!(member(&structure, name, arguments), expected, "{name}");
    }
    let segments = invoke(&structure, "segments", vec![]).expect("segments");
    assert_eq!(member(&segments, "size", vec![]), "4 (java.lang.Integer)");
    assert_eq!(
        member(&segments, "head", vec![]),
        "4M (com.fulcrumgenomics.util.ReadSegment)"
    );
    let barcode = invoke(&structure, "apply", integer(2)).expect("a segment");
    let length = invoke(&barcode, "length", vec![]).expect("a length");
    let kind = invoke(&barcode, "kind", vec![]).expect("a kind");
    let last = invoke(&structure, "apply", integer(3)).expect("a segment");
    let cases = [
        (&barcode, "offset", "7 (java.lang.Integer)"),
        (&barcode, "length", "Some(12) (scala.Some)"),
        (&length, "get", "12 (java.lang.Integer)"),
        (&kind, "toString", "B (java.lang.String)"),
        (&kind, "code", "B (java.lang.Character)"),
        (&barcode, "hasFixedLength", "true (java.lang.Boolean)"),
        (&barcode, "fixedLength", "12 (java.lang.Integer)"),
        (&last, "offset", "19 (java.lang.Integer)"),
        (&last, "length", "None (scala.None$)"),
        (
            &last,
            "fixedLength",
            "IllegalStateException: fixedLength called on variable length segment: +T",
        ),
    ];
    for (object, name, expected) in cases {
        assert_eq!(member(object, name, vec![]), expected, "{name}");
    }
    assert_eq!(
        kind.java_class_name(),
        "com.fulcrumgenomics.util.SegmentType$SampleBarcode$"
    );
    let fixed = function("readStructure", vec![Value::string("8M142T")]).expect("a structure");
    assert_eq!(
        member(&fixed, "fixedLength", vec![]),
        "150 (java.lang.Integer)"
    );
    assert_eq!(
        member(&fixed, "withVariableLastSegment", vec![]),
        "8M+T (com.fulcrumgenomics.util.ReadStructure)"
    );
    let same = function("readStructure", vec![Value::string("8m142t")]).expect("a structure");
    assert!(same.groovy_equals(&fixed), "equal by segments");
    assert!(!structure.groovy_equals(&fixed));
}

#[test]
fn extracts_short_reads() {
    let structure = function("readStructure", vec![Value::string("4M3S+T")]).expect("parses");
    let extract = |arguments: Vec<&str>| {
        let arguments = arguments.into_iter().map(Value::string).collect();
        shown(invoke(&structure, "extract", arguments))
    };
    assert_eq!(
        extract(vec!["AAA"]),
        "IllegalArgumentException: requirement failed: Read ends before end of segment: 4M"
    );
    assert_eq!(
        extract(vec!["AAAACCCGG", "II"]),
        "AssertionError: assertion failed"
    );
    assert_eq!(
        extract(vec!["AAAACCC"]),
        "Vector(SubReadWithoutQuals(AAAA,4M), SubReadWithoutQuals(CCC,3S), \
         SubReadWithoutQuals(,0T))"
    );
}
