//! Reads tools of tools-iuc with their macro files, and converts them into tool definitions.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::Path;

use galaxy_parser::definition::convert;
use galaxy_parser::tool::{self, Input, Tool};
use serde_json::{Value as Json, json};
use tool_definition::{Cardinality, Definition, TestValue, parameters};

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
fn mageck_definition() {
    let tool = fixture("mageck_test.xml");
    let formats = BTreeMap::from([("tabular".to_owned(), "format_3475".to_owned())]);
    let definition: Definition = convert(&tool, &formats, None);
    let names: Vec<&str> = definition
        .inputs
        .iter()
        .map(|port| port.name.as_str())
        .collect();
    assert_eq!(names, ["count_table", "adv|control_sgrna", "adv|cnv_norm"]);
    assert!(definition.inputs[0].required && !definition.inputs[1].required);
    assert_eq!(definition.inputs[0].formats, ["format_3475"]);
    assert_eq!(definition.inputs[0].cardinality, Cardinality::One);
    assert_eq!(
        definition
            .output("normcounts")
            .and_then(|output| output.created_when.as_deref()),
        Some("out['normcounts'] is True")
    );
    assert!(
        definition
            .output("gene_summary")
            .is_some_and(|output| output.created_when.is_none())
    );
    let first = &definition.tests[0].inputs;
    assert_eq!(
        first.get("sampleids|sample_select"),
        Some(&TestValue::Value(json!("treated"))),
        "inferred case"
    );
    assert_eq!(
        first.get("count_table"),
        Some(&TestValue::File("in.test.sample.txt".to_owned()))
    );
    let second = &definition.tests[1].inputs;
    assert_eq!(
        second.get("out|normcounts"),
        Some(&TestValue::Value(Json::Bool(true)))
    );
    let values = json!({"sampleids": {"sample_select": "control", "day0_label": "d0"},
                        "out": {"normcounts": true}, "adv": {"fdr_threshold": 0.1}});
    assert!(
        parameters::check(&definition.parameters, &values).is_empty(),
        "test values fit the schema"
    );
    let wrong = json!({"adv": {"fdr_threshold": 2.0, "norm_method": "mean"}});
    assert_eq!(parameters::check(&definition.parameters, &wrong).len(), 2);
}

#[test]
fn branch_ports_conditional() {
    let text = r#"<tool id="branch" name="Branch" version="1"><inputs>
        <conditional name="mode"><param name="layout" type="select">
            <option value="single"/><option value="paired"/></param>
            <when value="single"/>
            <when value="paired"><param name="mates" type="data" format="fastqsanger"/></when>
        </conditional></inputs></tool>"#;
    let tool =
        tool::parse(text, "branch.xml", &mut |_| unreachable!("no imports")).expect("a tool");
    let definition = convert(&tool, &BTreeMap::new(), None);
    let port = &definition.inputs[0];
    assert_eq!(port.name, "mode|mates");
    let condition = port.active_when.as_ref().expect("a branch condition");
    assert!(parameters::check(condition, &json!({"mode": {"layout": "paired"}})).is_empty());
    assert!(!parameters::check(condition, &json!({"mode": {"layout": "single"}})).is_empty());
    assert!(
        !parameters::check(condition, &json!({})).is_empty(),
        "the default case is single"
    );
    assert!(
        port.formats.is_empty(),
        "an unmapped datatype takes every format"
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
    let definition = convert(&tool, &BTreeMap::new(), None);
    let inputs = &definition.tests[0].inputs;
    assert_eq!(
        inputs.get("in_vcf"),
        Some(&TestValue::File("in.vcf".to_owned()))
    );
}
