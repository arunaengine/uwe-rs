//! nf-fgbio's read structures and sample sheets against nf-fgbio 1.0.1 with Nextflow 26.04.6.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_config::Config;
use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::value::{PathValue, Value};
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

/// The samples of a sheet as their texts, one per line, or the exception.
fn sheet(options: Option<Value>, path: &str) -> String {
    let mut arguments: Vec<Value> = options.into_iter().collect();
    arguments.push(Value::string(path));
    match function("fromSampleSheet", arguments) {
        Ok(Value::List(samples)) => {
            let samples = samples.borrow().items.clone();
            samples.iter().map(|s| format!("{s}\n")).collect()
        }
        other => shown(other),
    }
}

const SAMPLES: [&str; 3] = [
    "Sample(1,S1,Name_One,S1,Some(ProjA),Some(\"desc, with comma\"),Some(1),Some(ACGTACGT),\
     Some(TTGGCCAA),HashMap(OTHER -> y1, CUSTOM_COL -> x1))\n",
    "Sample(2,S2,Name_Two,S2,None,None,Some(1),Some(CCCCGGGG),None,\
     HashMap(OTHER -> , CUSTOM_COL -> x2))\n",
    "Sample(3,S1,Name_One,S1,Some(ProjB),Some(d3),Some(2),Some(GGGGAAAA),Some(AAAATTTT),\
     HashMap(OTHER -> , CUSTOM_COL -> ))\n",
];

#[test]
fn reads_sample_sheets() {
    let lane = |n: i32| Some(Value::map([(Value::string("lane"), Value::integer(n))]));
    assert_eq!(sheet(None, "sheet.csv"), SAMPLES.concat());
    assert_eq!(sheet(lane(0), "sheet.csv"), SAMPLES.concat());
    assert_eq!(
        sheet(lane(2), "sheet.csv"),
        SAMPLES[2].replacen("Sample(3,", "Sample(1,", 1)
    );
    assert_eq!(sheet(lane(3), "sheet.csv"), "");
    let unknown = Some(Value::map([(Value::string("foo"), Value::integer(1))]));
    assert_eq!(
        sheet(unknown, "sheet.csv"),
        "IllegalArgumentException: Unknown argument 'foo' for operator 'fromSampleSheet' -- \
         Possible arguments: lane"
    );
    let text = Some(Value::map([(Value::string("lane"), Value::string("2"))]));
    assert_eq!(
        sheet(text, "sheet.csv"),
        "IllegalArgumentException: Value '2' cannot be used in in parameter 'lane' for operator \
         'fromSampleSheet' -- Value don't match: class java.lang.Integer"
    );
    let path = Value::Path(PathValue::parse("sheet.csv", "/data").into());
    assert_eq!(
        shown(function("fromSampleSheet", vec![path])),
        "MissingMethodException: Missing process or function fromSampleSheet([/data/sheet.csv])"
    );
    assert_eq!(
        sheet(None, "nope.csv"),
        "FileNotFoundException: nope.csv (No such file or directory)"
    );
}

#[test]
fn checks_sheet_rules() {
    let cases = [
        (
            "badlane",
            "NumberFormatException: For input string: \"one\"",
        ),
        (
            "crlf",
            "Sample(1,S1,N1,S1,None,None,None,None,None,Map())\n",
        ),
        (
            "dupid",
            "IllegalArgumentException: requirement failed: Sample identifiers were not unique: S1",
        ),
        (
            "dupnamelib",
            "IllegalArgumentException: requirement failed: Sample name and library identifier \
             combinations were not unique: N1 L1",
        ),
        (
            "emptymid",
            "IllegalArgumentException: Found empty line on row 2 of the sample data section.",
        ),
        (
            "emptyname",
            "IllegalArgumentException: Sample at ordinal #1 is missing: Sample_Name",
        ),
        (
            "headerspace",
            "Sample(1,\"S1\",N1,\"S1\",None,None,None,Some(acgt),None,Map())\n",
        ),
        (
            "lowercase",
            "IllegalArgumentException: Could not find the header for sample data.",
        ),
        (
            "mismatch",
            "IllegalArgumentException: Found a line with a mismatching number of columns on \
             line #4: S2,N2,extra",
        ),
        (
            "mixedlane",
            "IllegalArgumentException: requirement failed: Sample identifiers were not unique: S1",
        ),
        (
            "nodata",
            "IllegalArgumentException: Could not find the header for sample data.",
        ),
        (
            "noheader",
            "IllegalArgumentException: Could not find the header for sample data.",
        ),
        (
            "noname",
            "IllegalArgumentException: Sample at ordinal #1 is missing: Sample_Name",
        ),
    ];
    for (name, expected) in cases {
        assert_eq!(sheet(None, &format!("{name}.csv")), expected, "{name}");
    }
}

#[test]
fn answers_sample_methods() {
    let Ok(Value::List(samples)) = function("fromSampleSheet", vec![Value::string("sheet.csv")])
    else {
        panic!("samples");
    };
    let samples = samples.borrow().items.clone();
    let (first, second) = (&samples[0], &samples[1]);
    let cases = [
        (first, "sampleOrdinal", "1 (java.lang.Integer)"),
        (first, "sampleId", "S1 (java.lang.String)"),
        (first, "sampleName", "Name_One (java.lang.String)"),
        (first, "libraryId", "S1 (java.lang.String)"),
        (first, "project", "Some(ProjA) (scala.Some)"),
        (
            first,
            "description",
            "Some(\"desc, with comma\") (scala.Some)",
        ),
        (first, "lane", "Some(1) (scala.Some)"),
        (first, "i7IndexBases", "Some(ACGTACGT) (scala.Some)"),
        (first, "i5IndexBases", "Some(TTGGCCAA) (scala.Some)"),
        (
            first,
            "extendedAttributes",
            "HashMap(OTHER -> y1, CUSTOM_COL -> x1) (scala.collection.immutable.HashMap)",
        ),
        (
            first,
            "sampleBarcodes",
            "List(Some(ACGTACGT), Some(TTGGCCAA)) (scala.collection.immutable.$colon$colon)",
        ),
        (
            first,
            "sampleBarcodeString",
            "ACGTACGT-TTGGCCAA (java.lang.String)",
        ),
        (second, "project", "None (scala.None$)"),
        (second, "i5IndexBases", "None (scala.None$)"),
        (
            second,
            "sampleBarcodeBases",
            "List(Some(CCCCGGGG), None) (scala.collection.immutable.$colon$colon)",
        ),
        (second, "sampleBarcodeString", "CCCCGGGG (java.lang.String)"),
    ];
    for (sample, name, expected) in cases {
        assert_eq!(member(sample, name, vec![]), expected, "{name}");
    }
    let attribute = |sample: &Value, name: &str| {
        shown(invoke(
            sample,
            "extendedAttribute",
            vec![Value::string(name)],
        ))
    };
    assert_eq!(attribute(first, "custom_col"), "Some(x1)");
    assert_eq!(attribute(first, "OTHER"), "Some(y1)");
    assert_eq!(attribute(second, "OTHER"), "None");
    let project = invoke(second, "project", vec![]).expect("an option");
    assert_eq!(
        member(&project, "isDefined", vec![]),
        "false (java.lang.Boolean)"
    );
    assert_eq!(
        shown(invoke(&project, "getOrElse", vec![Value::string("none")])),
        "none"
    );
}
