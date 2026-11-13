//! Checks writable Directory copies and refreshed downstream views after in-place updates.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_runtime::{CwlDriver, Options, Value};
use workflow_graph::{Driver, Launch, Outcome, OutputFile, Writable};

#[test]
fn refreshes_directory_views() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("removed.txt"), "original").unwrap();
    std::fs::write(
        root.path().join("job.json"),
        serde_json::json!({
            "data": {"class": "Directory", "location": cwl_parser::iri::file_url(&source)}
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        root.path().join("main.cwl"),
        r#"
cwlVersion: v1.2
class: Workflow
requirements: {InlineJavascriptRequirement: {}}
inputs: {data: Directory}
outputs:
  original: {type: 'File[]', outputSource: read/original}
  updated: {type: 'File[]', outputSource: read/updated}
steps:
  write:
    in: {data: data}
    out: [out]
    run:
      class: CommandLineTool
      requirements:
        InitialWorkDirRequirement:
          listing: [{entry: $(inputs.data), entryname: inp, writable: true}]
        InplaceUpdateRequirement: {inplaceUpdate: true}
      inputs: {data: Directory}
      outputs: {out: {type: Directory, outputBinding: {glob: inp}}}
      baseCommand: touch
      arguments: [inp/blurb]
  read:
    in: {old: data, new: write/out}
    out: [original, updated]
    run:
      class: ExpressionTool
      inputs:
        old: {type: Directory, loadListing: shallow_listing}
        new: {type: Directory, loadListing: shallow_listing}
      outputs: {original: 'File[]', updated: 'File[]'}
      expression: '$({original: inputs.old.listing, updated: inputs.new.listing})'
"#,
    )
    .unwrap();
    let mut driver = CwlDriver::new(
        &Launch {
            target: root.path().join("main.cwl"),
            inputs: Some(root.path().join("job.json")),
            ..Launch::default()
        },
        Options {
            simulated: false,
            ..Options::default()
        },
    )
    .unwrap();
    let tasks = driver.start().unwrap();
    assert_eq!(tasks.len(), 1);
    let task = &tasks[0];
    assert!(
        task.inputs.iter().any(|input| input.path == "inp"
            && input.directory
            && input.writable == Writable::Copy)
    );
    assert!(task.outputs.contains(&"inp/".to_owned()));
    let updated = root.path().join("work/inp");
    std::fs::create_dir_all(&updated).unwrap();
    std::fs::write(updated.join("blurb"), "updated").unwrap();
    assert!(
        driver
            .complete(Outcome {
                id: task.id,
                files: vec![OutputFile {
                    path: "inp".into(),
                    location: updated.to_string_lossy().into_owned(),
                    directory: true,
                    ..OutputFile::default()
                }],
                ..Outcome::default()
            })
            .unwrap()
            .is_empty()
    );
    let outputs = driver.outputs().unwrap();
    for name in ["original", "updated"] {
        let Value::List(listing) = &outputs[name] else {
            panic!("a listing")
        };
        assert_eq!(listing.len(), 1);
        assert_eq!(listing[0].text("basename"), Some("blurb"));
        assert_eq!(
            listing[0].text("location"),
            Some(cwl_parser::iri::file_url(&updated.join("blurb")).as_str())
        );
    }
    assert_eq!(
        std::fs::read_to_string(source.join("removed.txt")).unwrap(),
        "original"
    );
    assert!(!source.join("blurb").exists());
}
