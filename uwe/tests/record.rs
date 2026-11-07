//! Records a command with `uwe record` and plans the written tool with the simulator.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

#[test]
fn records_and_plans() {
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(folder.path().join("input.txt"), "hello\n").unwrap();
    let words = [
        "record",
        "-name",
        "copy",
        "--",
        "cat",
        "input.txt",
        ">",
        "output.txt",
    ];
    let output = (common::command().args(words).current_dir(folder.path()))
        .output()
        .unwrap();
    let printed = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(printed.contains("with 1 inputs and 1 outputs"), "{printed}");
    let tool = std::fs::read_to_string(folder.path().join("copy.cwl")).unwrap();
    assert!(tool.contains("stdout: output.txt"), "{tool}");
    let words = ["run", "copy.cwl", "-executor", "simulator"];
    let output = (common::command().args(words).current_dir(folder.path()))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn needs_command_line() {
    let folder = tempfile::tempdir().unwrap();
    let output = (common::command().args(["record", "-name", "copy"]))
        .current_dir(folder.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("uwe record [-name"));
}
