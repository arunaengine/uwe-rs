//! Reads tools of tools-iuc with their macro files into the typed model.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use galaxy_parser::tool::{self, Input, Tool};

fn fixture(name: &str) -> Tool {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    tool::load(&path).expect("a tools-iuc tool")
}

#[test]
fn mageck_model() {
    let tool = fixture("mageck_test.xml");
    assert_eq!(
        (
            tool.id.as_str(),
            tool.version.as_str(),
            tool.profile.as_deref()
        ),
        ("mageck_test", "0.5.9.2.2", Some("25.1"))
    );
    assert_eq!(tool.requirements.len(), 4);
    assert_eq!(tool.requirements[0].version.as_deref(), Some("0.5.9.2"));
    assert_eq!(tool.biotools.as_deref(), Some("mageck"));
    assert_eq!(tool.dois, ["10.1186/s13059-014-0554-4"]);
    assert_eq!(tool.detect_errors.as_deref(), Some("exit_code"));
    let command = tool.command.as_ref().expect("a command");
    assert!(
        command.text.starts_with("\n\nmageck test\n"),
        "{:?}",
        command.text
    );
    assert_eq!(command.line, 11);
    let Input::Section(advanced) = &tool.inputs[4] else {
        panic!("the advanced section: {:?}", tool.inputs[4]);
    };
    assert!(
        advanced
            .inputs
            .iter()
            .any(|input| matches!(input, Input::Param(p) if p.name == "sort_criteria")),
        "the sort_criteria macro is expanded into the section"
    );
    let log = &tool.outputs[2];
    assert_eq!(
        (log.name.as_str(), log.filters.as_slice()),
        ("log", &["out['out_log'] is True".to_owned()][..])
    );
    assert_eq!(tool.tests.len(), 2);
    let second = &tool.tests[1];
    assert!(
        second
            .attributes
            .contains(&("expect_num_outputs".to_owned(), "7".to_owned()))
    );
    assert_eq!(second.outputs.len(), 7, "every expected output is kept");
    let paths: Vec<&str> = second
        .params
        .iter()
        .map(|param| param.path.as_str())
        .collect();
    assert!(
        paths.contains(&"sampleids|treatment_id") && paths.contains(&"out|normcounts"),
        "{paths:?}"
    );
}

#[test]
fn get_hrun_macros() {
    let tool = fixture("get_hrun.xml");
    assert_eq!(
        (tool.version.as_str(), tool.profile.as_deref()),
        ("1.0+galaxy0", Some("20.01"))
    );
    let names: Vec<&str> = tool.requirements.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["pyfaidx", "pyvcf", "samtools"]);
    assert_eq!(tool.requirements[0].version.as_deref(), Some("0.5.9.2"));
    let params: Vec<&str> = tool.tests[0]
        .params
        .iter()
        .map(|p| p.path.as_str())
        .collect();
    assert_eq!(params, ["in_vcf", "reference"]);
}
