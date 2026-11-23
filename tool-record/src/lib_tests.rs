//! Records real commands in a temporary folder and loads the written tools with cwl-parser.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::*;
use cwl_parser::model::Class;
use cwl_parser::source::Remote;

fn request(folder: &Path, line: &str) -> Request {
    Request {
        command: line.split_whitespace().map(str::to_owned).collect(),
        name: None,
        image: None,
        folder: folder.to_owned(),
        format: Format::Cwl,
    }
}

/// The last part of each id of a loaded tool, as the CWL file names it.
fn names<'a>(ids: impl Iterator<Item = &'a String>) -> Vec<String> {
    let last = |id: &String| id.rsplit(['#', '/']).next().unwrap_or(id).to_owned();
    ids.map(last).collect()
}

#[test]
fn records_redirected_cat() {
    let folder = tempfile::tempdir().unwrap();
    fs::write(folder.path().join("input.txt"), "hello\n").unwrap();
    let mut asked = request(folder.path(), "cat input.txt > output.txt");
    asked.image = Some("docker.io/library/busybox:1.37.0".into());
    let recorded = record(&asked).unwrap();
    assert_eq!(recorded.path, folder.path().join("cat.cwl"));
    assert_eq!(
        fs::read_to_string(folder.path().join("output.txt")).unwrap(),
        "hello\n"
    );
    let tool = &recorded.tool;
    assert_eq!(tool.stdout.as_deref(), Some("output.txt"));
    assert_eq!(tool.outputs.len(), 1, "only the new file is an output");
    assert_eq!(
        (tool.outputs[0].id.as_str(), tool.outputs[0].kind),
        ("output_txt", Kind::File)
    );
    let loaded = cwl_parser::load_file(&recorded.path, Remote::default()).unwrap();
    let process = &loaded.process;
    assert!(matches!(process.class, Class::CommandLineTool(_)));
    assert_eq!(
        names(process.inputs.iter().map(|input| &input.id)),
        ["input_txt"]
    );
    assert_eq!(
        names(process.outputs.iter().map(|output| &output.id)),
        ["output_txt"]
    );
    assert_eq!(process.hints.len(), 1);
}

#[test]
fn writes_tool_definitions() {
    let folder = tempfile::tempdir().unwrap();
    fs::write(folder.path().join("reads.fastq"), "@r\nA\n+\nI\n").unwrap();
    let mut asked = request(folder.path(), "cp reads.fastq copy.fastq");
    asked.format = Format::Both;
    let recorded = record(&asked).unwrap();
    assert_eq!(recorded.path, folder.path().join("cp.cwl"));
    let file = recorded.definition.expect("a definition file");
    assert_eq!(file, folder.path().join("cp.tool.json"));
    let text = fs::read_to_string(&file).unwrap();
    let definition: tool_definition::Definition = serde_json::from_str(&text).unwrap();
    assert_eq!(definition.id, "cp");
    let source = definition.source.clone().expect("the CWL file runs it");
    assert_eq!(
        (source.kind, source.address.as_str()),
        (SourceKind::Cwl, "cp.cwl")
    );
    assert_eq!(
        definition.inputs[0].formats,
        ["format_1930"],
        "FASTQ by its ending"
    );
    let output = definition.outputs.first().expect("the copy is an output");
    assert_eq!(
        output.format,
        tool_definition::OutputFormat::Terms(vec!["format_1930".into()])
    );
    let only = tempfile::tempdir().unwrap();
    let mut asked = request(only.path(), "touch made.txt");
    asked.format = Format::Tool;
    let recorded = record(&asked).unwrap();
    assert_eq!(recorded.path, only.path().join("touch.tool.json"));
    assert!(!only.path().join("touch.cwl").exists(), "no CWL file");
}

#[test]
fn keeps_command_files() {
    let folder = tempfile::tempdir().unwrap();
    let mut asked = request(folder.path(), "touch made.tool.json");
    asked.name = Some("made".into());
    asked.format = Format::Both;
    let error = record(&asked).expect_err("the command made the definition file");
    assert!(matches!(error, Error::Exists(_)), "{error}");
    assert_eq!(fs::read(folder.path().join("made.tool.json")).unwrap(), b"");
    assert!(
        !folder.path().join("made.cwl").exists(),
        "the CWL file of the pair goes"
    );
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("target.txt");
    fs::write(&target, "kept\n").unwrap();
    let mut asked = request(
        folder.path(),
        &format!("ln -s {} linked.cwl", target.display()),
    );
    asked.name = Some("linked".into());
    let error = record(&asked).expect_err("the command made a link");
    assert!(matches!(error, Error::Exists(_)), "{error}");
    assert_eq!(fs::read_to_string(&target).unwrap(), "kept\n");
}

#[test]
fn records_two_inputs() {
    let folder = tempfile::tempdir().unwrap();
    fs::write(folder.path().join("left.txt"), "a\nb\n").unwrap();
    fs::write(folder.path().join("right.txt"), "1\n2\n").unwrap();
    fs::create_dir(folder.path().join(".git")).unwrap();
    let mut asked = request(folder.path(), "paste -d , left.txt right.txt > pairs.csv");
    asked.name = Some("pair.cwl".into());
    let recorded = record(&asked).unwrap();
    assert_eq!(recorded.path, folder.path().join("pair.cwl"));
    let kinds: Vec<(&str, Kind)> = (recorded.tool.inputs.iter())
        .map(|input| (input.id.as_str(), input.kind))
        .collect();
    assert_eq!(
        kinds,
        [
            ("d", Kind::String),
            ("left_txt", Kind::File),
            ("right_txt", Kind::File)
        ]
    );
    let output = &recorded.tool.outputs[0];
    assert_eq!(
        (output.id.as_str(), output.glob.as_str()),
        ("pairs_csv", "pairs.csv")
    );
    let loaded = cwl_parser::load_file(&recorded.path, Remote::default()).unwrap();
    assert_eq!(loaded.process.inputs.len(), 3);
}

#[test]
fn takes_new_folders() {
    let folder = tempfile::tempdir().unwrap();
    let recorded = record(&request(folder.path(), "mkdir -p results/plots")).unwrap();
    let outputs: Vec<(&str, Kind)> = (recorded.tool.outputs.iter())
        .map(|output| (output.glob.as_str(), output.kind))
        .collect();
    assert_eq!(outputs, [("results", Kind::Directory)]);
    cwl_parser::load_file(&recorded.path, Remote::default()).unwrap();
}

#[test]
fn failure_writes_nothing() {
    let folder = tempfile::tempdir().unwrap();
    let error = record(&request(folder.path(), "false")).unwrap_err();
    assert!(matches!(error, Error::Failed { .. }), "{error}");
    assert!(!folder.path().join("false.cwl").exists());
}

#[test]
fn refuses_taken_names() {
    let folder = tempfile::tempdir().unwrap();
    fs::write(folder.path().join("echo.cwl"), "").unwrap();
    let taken = record(&request(folder.path(), "echo hi")).unwrap_err();
    assert!(matches!(taken, Error::Exists(_)), "{taken}");
    let mut nested = request(folder.path(), "echo hi");
    nested.name = Some("tools/echo".into());
    assert!(matches!(record(&nested), Err(Error::Name(_))));
    assert!(matches!(
        record(&request(folder.path(), "")),
        Err(Error::Empty)
    ));
}
