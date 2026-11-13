//! nf-ped's PED import and export against nf-ped 0.4.0 with Nextflow 26.04.6.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::rc::Rc;

use nextflow_config::Config;
use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::value::{PathValue, Value};
use nextflow_plugins::{Context, call, method};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/nf_ped");

/// Runs in a folder of its own, which is also the folder of the run's own files.
struct Folder(String);

impl Host for Folder {
    fn print(&mut self, _: &str) {}

    fn launch_dir(&self) -> String {
        self.0.clone()
    }

    fn operator_directory(&mut self) -> Result<PathValue, Error> {
        Ok(PathValue::parse(&self.0, "/"))
    }
}

fn fixture(name: &str) -> Value {
    Value::Path(Rc::new(PathValue::parse(
        &format!("{FIXTURES}/{name}"),
        "/",
    )))
}

fn with_context<T>(folder: &Path, run: impl FnOnce(&mut Context) -> T) -> T {
    let config = Config::default();
    let mut host = Folder(folder.display().to_string());
    let mut context = Context {
        host: &mut host,
        config: &config,
        params: Value::map([]),
        project_dir: "/",
        cli_params: &[],
        workflow: Value::Null,
    };
    run(&mut context)
}

/// The entries of a file as the probe prints them, or the error message.
fn imported(name: &str) -> String {
    let folder = tempfile::tempdir().expect("a folder");
    with_context(folder.path(), |context| {
        let pedigree = match call("nf-ped", "initializePed", vec![fixture(name)], context) {
            Ok(pedigree) => pedigree,
            Err(Error::Thrown { exception, .. }) => {
                return exception.message.replace(FIXTURES, "<probe-dir>");
            }
            Err(other) => panic!("{other}"),
        };
        let entries = method(&pedigree, "getEntries", Vec::new(), context);
        let Ok(Some(Value::Set(entries))) = entries else {
            panic!("a set of entries");
        };
        let entries: Vec<Value> = entries.borrow().items.iter().map(|k| k.0.clone()).collect();
        entries
            .iter()
            .map(|e| format!("[{}]\n", e.to_string().replace('\t', "|")))
            .collect()
    })
}

#[test]
fn imports_files() {
    let cases = [
        (
            "blankline",
            "Could not determine PED entry at line 2 in 'file://<probe-dir>/blankline.ped': \
             expected at least6 columns, found 1",
        ),
        ("crlf", "[f1|i1|0|0|1|2]\n[f1|i2|0|0|2|1]\n"),
        ("emptytrail", "[f1|i1|0|0|1|2]\n"),
        (
            "fewcols",
            "Could not determine PED entry at line 1 in 'file://<probe-dir>/fewcols.ped': \
             expected at least6 columns, found 5",
        ),
        (
            "nofamily",
            "Expected a family ID at line 2 in PED file 'file://<probe-dir>/nofamily.ped'",
        ),
        (
            "noindividual",
            "Expected an individual ID at line 1 in PED file 'file://<probe-dir>/noindividual.ped'",
        ),
        ("sexwords", "[f1|i1|0|0|0|0]\n"),
        (
            "spaces",
            "Could not determine PED entry at line 1 in 'file://<probe-dir>/spaces.ped': \
             expected at least6 columns, found 1",
        ),
        (
            "trailingtab",
            "Could not determine PED entry at line 1 in 'file://<probe-dir>/trailingtab.ped': \
             expected at least6 columns, found 5",
        ),
    ];
    for (name, expected) in cases {
        assert_eq!(imported(&format!("{name}.ped")), expected, "{name}");
    }
}

#[test]
fn writes_files() {
    let folder = tempfile::tempdir().expect("a folder");
    with_context(folder.path(), |context| {
        let pedigree =
            call("nf-ped", "initializePed", vec![fixture("a.ped")], context).expect("a pedigree");
        let mut write = |arguments: Vec<Value>| {
            let written = method(&pedigree, "writePed", arguments, context).expect("writes");
            written.expect("a method").to_string()
        };
        let families = |names: &[&str]| {
            let names = names.iter().map(|name| Value::string(*name)).collect();
            Value::map([(Value::string("families"), Value::list(names))])
        };
        let all = write(Vec::new());
        assert!(all.ends_with("/generated_peds/ped_eb8d9bab64a1a66766f9e111ccedd4b6.ped"));
        let some = write(vec![families(&["fam2", "famX"])]);
        assert!(some.ends_with("/generated_peds/ped_d19e7a8076f4ee8f2a813f6870cc1965.ped"));
        assert_eq!(write(vec![families(&["famX"])]), "null");
        let read = |path: &str| std::fs::read_to_string(path).expect("reads");
        let recorded = |name: &str| read(&format!("{FIXTURES}/{name}"));
        assert_eq!(read(&all), recorded("written-all.txt"));
        assert_eq!(read(&some), recorded("written-families.txt"));
        let explicit = write(vec![Value::string("out/explicit.ped")]);
        assert_eq!(
            explicit,
            folder.path().join("out/explicit.ped").display().to_string()
        );
        assert_eq!(read(&explicit), recorded("written-all.txt"));
    });
}
