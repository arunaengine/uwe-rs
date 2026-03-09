//! nf-schema functions against the plugin nf-schema 2.5.1 with Nextflow 26.04.6; the fixtures
//! have the script that recorded the expected values.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use nextflow_config::Config;
use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::value::Value;
use nextflow_plugins::{Context, call};

/// A host whose launch directory is the fixture directory.
struct Fixtures(String);

impl Host for Fixtures {
    fn print(&mut self, _: &str) {}

    fn launch_dir(&self) -> String {
        self.0.clone()
    }
}

fn directory() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/samplesheet")
        .to_string_lossy()
        .into_owned()
}

fn to_list(sheet: &str, schema: &str) -> Result<String, Error> {
    let directory = directory();
    let mut host = Fixtures(directory.clone());
    let config = Config::default();
    let mut context = Context {
        host: &mut host,
        config: &config,
        params: Value::map([]),
        project_dir: &directory,
    };
    let arguments = vec![Value::string(sheet), Value::string(schema)];
    call("nf-schema", "samplesheetToList", arguments, &mut context).map(|rows| rows.to_string())
}

#[test]
fn samplesheets_become_lists() {
    let dir = directory();
    assert_eq!(
        to_list("sheet.csv", "schema.json").unwrap(),
        format!(
            "[[[id:S1, group:1, grp:1, flag:true, note:hello], /data/a_1.fq.gz, /data/a_2.fq.gz, 1.5], \
             [[id:S2, group:[], grp:[], flag:false, note:[]], {dir}/b_1.fq.gz, [], []]]"
        )
    );
    assert_eq!(
        to_list("sheet2.csv", "schema2.json").unwrap(),
        format!("[[{dir}/a.fq, 7], [{dir}/b.fq, 3]]")
    );
    assert_eq!(
        to_list("sheet2.csv", "schema3.json").unwrap(),
        format!("[[{dir}/a.fq], [{dir}/b.fq]]")
    );
    assert_eq!(
        to_list("sheet.tsv", "schema.json").unwrap(),
        format!("[[[id:S1, group:[], grp:[], flag:[], note:[]], {dir}/x.fq, [], []]]")
    );
}

#[test]
fn invalid_samplesheets_fail() {
    let error = to_list("bad.csv", "schema.json").unwrap_err().to_string();
    assert!(
        error.starts_with("SchemaValidationException: Validation of samplesheet failed!"),
        "{error}"
    );
    assert!(
        error.contains("-> Entry 1: Error for field 'sample'"),
        "{error}"
    );
    assert!(
        error.contains("\"fastq_1\" is a required property"),
        "{error}"
    );
}
