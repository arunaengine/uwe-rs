//! Renders tier 1 tools of tools-iuc from their test values against hand-written commands.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::Path;

use cheetah_evaluator::{DataFile, Job, render};
use galaxy_parser::definition::convert;
use galaxy_parser::tool::{self, Tool};
use serde_json::{Value as Json, json};
use tool_definition::TestValue;

/// The commands below follow Galaxy's rules: directive lines vanish, each line is stripped,
/// and lines are joined with spaces, so each empty line adds one space.
const MAGECK_DEFAULT: &str = "mageck test -k 'inputs/in.test.sample.txt'  \
-t 'HL60_final,KBM7_final'  -c 'HL60_initial,KBM7_initial'  -n output      \
--norm-method median   --adjust-method fdr    --sort-criteria neg  --remove-zero both   \
--gene-lfc-method median";

fn fixture(name: &str) -> Tool {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../parser/tests/fixtures")
        .join(name);
    tool::load(&path).expect("a tools-iuc tool")
}

/// The job of a tool test: values nested by their paths, files under `inputs/`, outputs
/// under `outputs/`.
fn job(tool: &Tool, test: usize) -> Job {
    let definition = convert(tool, &BTreeMap::new(), None);
    let mut job = Job {
        parameters: json!({}),
        tool_directory: "/tools/get_hrun".to_owned(),
        ..Job::default()
    };
    for (path, value) in &definition.tests[test].inputs {
        match value {
            TestValue::Value(value) => set(&mut job.parameters, path, value.clone()),
            TestValue::File(name) => {
                let file = DataFile {
                    path: format!("inputs/{name}"),
                    ext: "data".to_owned(),
                    name: name.clone(),
                };
                job.datasets.insert(path.clone(), vec![file]);
            }
            TestValue::Files(_) => panic!("no tier 1 test gives several files"),
        }
    }
    for output in &tool.outputs {
        job.outputs
            .insert(output.name.clone(), format!("outputs/{}", output.name));
    }
    job
}

/// Sets a value at a path like `out|normcounts`, or removes it for null.
fn set(parameters: &mut Json, path: &str, value: Json) {
    let mut parts: Vec<&str> = path.split('|').collect();
    let last = parts.pop().expect("a name");
    let mut object = parameters;
    for part in parts {
        object = object
            .as_object_mut()
            .expect("an object")
            .entry(part)
            .or_insert_with(|| json!({}));
    }
    let entries = object.as_object_mut().expect("an object");
    match value {
        Json::Null => entries.remove(last),
        value => entries.insert(last.to_owned(), value),
    };
}

#[test]
fn mageck_test_values() {
    let tool = fixture("mageck_test.xml");
    let rendered = render(&tool, &job(&tool, 0)).expect("renders");
    assert_eq!(rendered.command.as_deref(), Some(MAGECK_DEFAULT));
    assert_eq!(rendered.created, ["gene_summary", "sgrna_summary"]);
    let all = render(&tool, &job(&tool, 1)).expect("renders");
    let flags = MAGECK_DEFAULT.replace(
        "-n output      ",
        "-n output  --normcounts-to-file  --pdf-report  ",
    );
    assert_eq!(all.command.as_deref(), Some(flags.as_str()));
    let created = [
        "gene_summary",
        "sgrna_summary",
        "log",
        "normcounts",
        "plots",
        "rscript",
        "rnwfile",
    ];
    assert_eq!(all.created, created, "the second test expects 7 outputs");
}

#[test]
fn mageck_empty_control() {
    let tool = fixture("mageck_test.xml");
    let mut job = job(&tool, 0);
    set(&mut job.parameters, "control_id", Json::Null);
    let rendered = render(&tool, &job).expect("renders");
    let expected = MAGECK_DEFAULT.replace("-c 'HL60_initial,KBM7_initial'", "-c ''");
    assert_eq!(
        rendered.command.as_deref(),
        Some(expected.as_str()),
        "'$control_id' in #if is a string literal, so the branch always renders"
    );
}

#[test]
fn mageck_normcounts_output() {
    let tool = fixture("mageck_test.xml");
    let mut on = job(&tool, 0);
    set(&mut on.parameters, "out|normcounts", Json::Bool(true));
    let rendered = render(&tool, &on).expect("renders");
    let expected = MAGECK_DEFAULT.replace("-n output      ", "-n output  --normcounts-to-file    ");
    assert_eq!(rendered.command.as_deref(), Some(expected.as_str()));
    assert!(
        rendered.created.iter().any(|name| name == "normcounts"),
        "{rendered:?}"
    );
    let mut off = job(&tool, 0);
    set(&mut off.parameters, "out|normcounts", Json::Bool(false));
    let rendered = render(&tool, &off).expect("renders");
    assert_eq!(rendered.command.as_deref(), Some(MAGECK_DEFAULT));
    assert!(
        !rendered.created.iter().any(|name| name == "normcounts"),
        "{rendered:?}"
    );
}

#[test]
fn get_hrun_command() {
    let tool = fixture("get_hrun.xml");
    let rendered = render(&tool, &job(&tool, 0)).expect("renders");
    let expected = "ln -s 'inputs/reference.fasta' ref.fa && samtools faidx ref.fa && \
python '/tools/get_hrun/get_hrun.py' --reference ref.fa --in-vcf 'inputs/in.vcf' \
--out-vcf 'outputs/out_vcf'";
    assert_eq!(rendered.command.as_deref(), Some(expected));
    assert_eq!(rendered.created, ["out_vcf"]);
}
