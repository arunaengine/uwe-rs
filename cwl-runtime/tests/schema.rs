//! The inputs and outputs of CWL processes as JSON Schema (decision 24).
// Copyright (c) 2026 The iunctus Contributors
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
fn a_tool_describes_its_inputs_and_outputs() {
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
fn nested_record_files_keep_formats_and_secondary_files() {
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
