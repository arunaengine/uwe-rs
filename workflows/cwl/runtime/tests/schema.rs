//! The inputs and outputs of CWL processes as JSON Schema (decision 24).
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_runtime::{CwlDriver, Options};
use serde_json::json;
use workflow_graph::{Driver, Launch};

const TOOL: &str = r#"cwlVersion: v1.2
class: CommandLineTool
label: count
doc: Counts the reads.
$namespaces: {edam: "http://edamontology.org/"}
baseCommand: wc
inputs:
  reads:
    type: File
    format: edam:format_1930
    inputBinding: {position: 1}
  mode:
    type: {type: enum, symbols: [lines, words]}
    default: lines
  label: string?
outputs:
  counts:
    type: File
    label: the counts
    outputBinding: {glob: counts.txt}
  log: File?
"#;

#[test]
fn describes_tool_inputs() {
    let folder = tempfile::tempdir().expect("a folder");
    let tool = folder.path().join("count.cwl");
    std::fs::write(&tool, TOOL).expect("writes the tool");
    let launch = Launch {
        target: tool,
        params: vec![("reads".into(), "missing.fq".into())],
        ..Launch::default()
    };
    let mut driver = CwlDriver::new(&launch, Options::default()).expect("loads the tool");
    let inputs = driver.inputs().expect("describes").expect("inputs");
    let schema = &inputs.schema;
    assert_eq!(schema["title"], "count");
    assert_eq!(schema["description"], "Counts the reads.");
    assert_eq!(schema["required"], json!(["reads"]));
    assert_eq!(
        schema["properties"]["reads"],
        json!({
            "type": "string",
            "format": "file-path",
            "exists": true,
            "fileFormat": "http://edamontology.org/format_1930"
        })
    );
    assert_eq!(
        schema["properties"]["mode"],
        json!({"type": "string", "enum": ["lines", "words"], "default": "lines"})
    );
    assert!(
        inputs.values["reads"]
            .as_str()
            .is_some_and(|path| path.ends_with("missing.fq"))
    );
    assert_eq!(inputs.read, Some(["reads".to_owned()].into()));
    // The driver's own `outputs` is the output object of a finished run.
    let outputs = Driver::outputs(&mut driver)
        .expect("describes")
        .expect("outputs");
    assert_eq!(outputs["required"], json!(["counts"]));
    assert_eq!(
        outputs["properties"]["counts"],
        json!({"type": "string", "format": "file-path", "title": "the counts"})
    );
    assert_eq!(input_schema::valid(&outputs), Ok(()));
}

#[test]
fn preserves_nested_files() {
    let folder = tempfile::tempdir().expect("a folder");
    let tool = folder.path().join("record.cwl");
    let source = r#"cwlVersion: v1.2
class: CommandLineTool
$namespaces: {edam: "http://edamontology.org/"}
baseCommand: 'true'
inputs:
  bundle:
    type:
      - 'null'
      - type: record
        fields:
          primary:
            type: ['null', File]
            doc: The primary read.
            format: edam:format_1930
            secondaryFiles: [.bai]
          reads:
            type: {type: array, items: File}
            format: [edam:format_1931, edam:format_1932]
            secondaryFiles: [.idx]
outputs: []
"#;
    std::fs::write(&tool, source).expect("writes the tool");
    let launch = Launch {
        target: tool,
        ..Launch::default()
    };
    let mut driver = CwlDriver::new(&launch, Options::default()).expect("loads the tool");
    let inputs = driver.inputs().expect("describes").expect("inputs");
    let bundle = &inputs.schema["properties"]["bundle"];
    assert_eq!(inputs.schema.get("required"), None);
    assert_eq!(bundle["required"], json!(["reads"]));
    assert_eq!(
        bundle["properties"]["primary"],
        json!({
            "type": "string",
            "format": "file-path",
            "exists": true,
            "description": "The primary read.",
            "fileFormat": "http://edamontology.org/format_1930",
            "secondaryFiles": [".bai"]
        })
    );
    assert_eq!(
        bundle["properties"]["reads"],
        json!({
            "type": "array",
            "items": {
                "type": "string",
                "format": "file-path",
                "exists": true,
                "fileFormat": [
                    "http://edamontology.org/format_1931",
                    "http://edamontology.org/format_1932"
                ],
                "secondaryFiles": [".idx"]
            }
        })
    );
    assert_eq!(input_schema::valid(&inputs.schema), Ok(()));
}

#[test]
fn preserves_long_defaults() {
    let folder = tempfile::tempdir().expect("a folder");
    let tool = folder.path().join("longs.cwl");
    let source = r#"cwlVersion: v1.2
class: CommandLineTool
baseCommand: 'true'
inputs:
  ordinary: {type: long, default: 42}
  precise: {type: long, default: 9007199254740993}
  minimum: {type: long, default: -9223372036854775808}
  maximum: {type: long, default: 9223372036854775807}
outputs: []
"#;
    std::fs::write(&tool, source).expect("writes the tool");
    let launch = Launch {
        target: tool,
        params: vec![
            ("ordinary".into(), "42".into()),
            ("precise".into(), "9007199254740993".into()),
            ("minimum".into(), "-9223372036854775808".into()),
            ("maximum".into(), "9223372036854775807".into()),
        ],
        ..Launch::default()
    };
    let mut driver = CwlDriver::new(&launch, Options::default()).expect("loads the tool");
    let inputs = driver.inputs().expect("describes").expect("inputs");
    for (name, expected) in [
        ("ordinary", 42),
        ("precise", 9_007_199_254_740_993),
        ("minimum", i64::MIN),
        ("maximum", i64::MAX),
    ] {
        assert_eq!(
            inputs.schema["properties"][name]["default"].as_i64(),
            Some(expected)
        );
        assert_eq!(inputs.values[name].as_i64(), Some(expected));
    }
    let text = serde_json::to_string(&inputs.schema).expect("schema text");
    assert!(text.contains("9007199254740993"), "{text}");
    assert!(!text.contains("9007199254740992"), "{text}");
    assert_eq!(input_schema::valid(&inputs.schema), Ok(()));
}

#[test]
fn rejects_integer_overflow() {
    for digits in ["9223372036854775808", "-9223372036854775809"] {
        let folder = tempfile::tempdir().expect("a folder");
        let tool = folder.path().join("outside.cwl");
        let source = format!(
            "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: 'true'\n\
             inputs:\n  value: {{type: long, default: {digits}}}\noutputs: []\n"
        );
        std::fs::write(&tool, source).expect("writes the tool");
        let launch = Launch {
            target: tool,
            ..Launch::default()
        };
        let mut driver = CwlDriver::new(&launch, Options::default()).expect("loads the tool");
        let error = driver
            .inputs()
            .expect_err("the integer is outside CWL long bounds");
        assert!(error.to_string().contains(digits), "{error}");
        assert!(
            error
                .to_string()
                .contains("outside the range of a CWL long"),
            "{error}"
        );
    }
}
