//! nf-iridanext against the output files that nf-iridanext 0.3.0 wrote with Nextflow 26.04.6:
//! gzip output with CSV metadata, JSON metadata with `flatten`, and the overwrite guard.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::io::Read;
use std::path::Path;

/// The config of the probe runs, with the output folder in `folder`.
fn config(folder: &Path, output: &str, samples: &str) -> String {
    format!(
        "plugins {{ id 'nf-iridanext@0.3.0' }}\n\
         params.outdir = '{}/results'\n\
         iridanext {{\n  enabled = true\n  output {{\n    {output}\n    validate = true\n    \
         files {{\n      global = ['**/summary/summary.txt.gz']\n      \
         samples = ['**/assembly/*.assembly.fa.gz']\n    }}\n    \
         metadata {{\n      samples {{\n        {samples}\n      }}\n    }}\n  }}\n}}\n",
        folder.display()
    )
}

fn json(text: &str) -> serde_json::Value {
    serde_json::from_str(text).expect("JSON")
}

#[test]
fn writes_csv_metadata() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let fixtures = common::fixtures("nf_iridanext");
    common::copy(&fixtures, folder, &["main.nf"]);
    let output = format!(
        "path = '{}/results/iridanext.output.json.gz'\n    overwrite = true",
        folder.display()
    );
    let samples = "rename = ['c': 'c_col']\n        \
                   csv { path = '**/meta/output.csv'; idcol = 'column1' }";
    let text = config(folder, &output, samples);
    std::fs::write(folder.join("nextflow.config"), text).expect("writes the config");
    let (execution, printed) = common::run(folder, "irida");
    assert!(
        execution.error.is_none(),
        "{:?}\n{printed}",
        execution.error
    );
    assert!(printed.contains("loaded [[id:SAMPLE3], x]"), "{printed}");
    let file = std::fs::File::open(folder.join("results/iridanext.output.json.gz"))
        .expect("the output file");
    let mut written = String::new();
    flate2::read::GzDecoder::new(file)
        .read_to_string(&mut written)
        .expect("gzip");
    let recorded = std::fs::read_to_string(fixtures.join("run1.iridanext.output.json"))
        .expect("the recorded output");
    // The samples of the recorded run finished in another order.
    assert_eq!(json(&written), json(&recorded), "{written}");
    assert!(!written.ends_with('\n'), "no newline at the end");
}

#[test]
fn flattens_json_metadata() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let fixtures = common::fixtures("nf_iridanext");
    common::copy(&fixtures, folder, &["main.nf"]);
    let target = folder.join("results/iridanext.output.json");
    let output = format!("path = '{}'\n    overwrite = false", target.display());
    let samples = "rename = ['c': 'c_col']\n        \
                   csv { path = '**/meta/output.csv'; idcol = 'column1' }\n        \
                   flatten = true\n        ignore = ['k.y.1']\n        \
                   json { path = '**/meta/output.json' }";
    let text = config(folder, &output, samples);
    std::fs::write(folder.join("nextflow.config"), text).expect("writes the config");
    let (execution, printed) = common::run(folder, "irida");
    assert!(
        execution.error.is_none(),
        "{:?}\n{printed}",
        execution.error
    );
    let written = std::fs::read_to_string(&target).expect("the output file");
    let recorded = std::fs::read_to_string(fixtures.join("run2.iridanext.output.json"))
        .expect("the recorded output");
    assert_eq!(written, recorded);
    std::fs::write(&target, "kept").expect("marks the file");
    let (execution, printed) = common::run(folder, "again");
    assert!(
        execution.error.is_none(),
        "the guard does not fail the run: {:?}\n{printed}",
        execution.error
    );
    let kept = std::fs::read_to_string(&target).expect("the output file");
    assert_eq!(kept, "kept", "overwrite = false keeps the file");
}

#[test]
fn rejects_unknown_parser() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let fixtures = common::fixtures("nf_iridanext");
    common::copy(&fixtures, folder, &["main.nf"]);
    let text = config(folder, "overwrite = true", "xml { path = '*.xml' }");
    std::fs::write(folder.join("nextflow.config"), text).expect("writes the config");
    let (execution, _) = common::run(folder, "irida");
    let error = execution.error.expect("the run stops").to_string();
    assert!(
        error
            .contains("Invalid type=xml for iridanext.output.metadata.samples: [xml:[path:*.xml]]"),
        "{error}"
    );
}

#[test]
fn refuses_remote_output() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let fixtures = common::fixtures("nf_iridanext");
    common::copy(&fixtures, folder, &["main.nf"]);
    let text = config(folder, "path = 's3://bucket/run/out.json'", "");
    std::fs::write(folder.join("nextflow.config"), text).expect("writes the config");
    let (execution, _) = common::run(folder, "irida");
    let error = execution.error.expect("the run stops").to_string();
    let expected = "nf-iridanext writes iridanext.output.path only to a local file, not \
                    s3://bucket/run/out.json";
    assert!(error.contains(expected), "{error}");
    // The run launches in the current folder, where the old code wrote `s3:/bucket/run`.
    assert!(!Path::new("s3:").exists(), "a local remote folder");
}
