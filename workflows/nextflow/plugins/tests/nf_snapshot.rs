//! nf-snapshot against the snapshot and the run messages that nf-snapshot 0.3.0 gave with
//! Nextflow 26.04.6 for the same pipeline.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::path::Path;

fn fixture(name: &str) -> String {
    std::fs::read_to_string(common::fixtures("nf_snapshot").join(name)).expect("a fixture")
}

/// Writes the probe with the recorded config, its outputs below the test's folder, and the
/// `params` lines in `extra`.
fn prepare(folder: &Path, extra: &str) {
    common::copy(&common::fixtures("nf_snapshot"), folder, &["main.nf"]);
    let config = format!(
        "plugins {{ id 'nf-snapshot@0.3.0' }}\nparams.outdir = '{}/results'\n{extra}\
         snapshot {{\n    enabled  = true\n    file     = 'tests/snapshot.json'\n    \
         nameOnly = ['**/*.log']\n    sizeOnly = ['**/*.dat']\n}}\n",
        folder.display()
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
}

/// The error of a run, `None` when it succeeded.
fn run(folder: &Path) -> Option<String> {
    let (execution, _) = common::run(folder, "snapshot");
    execution.error.map(|error| error.to_string())
}

/// The error message of a recorded run, with the test's folder for the launch directory.
fn recorded_error(name: &str, folder: &Path) -> String {
    let text = fixture(name);
    let start = text.find("nf-snapshot: outputs differ").expect("the error");
    let end = text.find("accept the changes").expect("its end") + "accept the changes".len();
    text[start..end].replace("/LAUNCH", &folder.display().to_string())
}

#[test]
fn writes_then_matches() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    prepare(folder, "");
    assert_eq!(run(folder), None);
    let written = std::fs::read_to_string(folder.join("tests/snapshot.json")).expect("written");
    assert_eq!(written, fixture("snapshot-created.json"));
    assert_eq!(run(folder), None, "the second run matches");
}

#[test]
fn fails_on_differences() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    prepare(folder, "");
    std::fs::create_dir_all(folder.join("tests")).expect("makes the folder");
    let edited = fixture("snapshot-edited.json");
    std::fs::write(folder.join("tests/snapshot.json"), &edited).expect("writes the snapshot");
    let error = run(folder).expect("the run fails");
    assert_eq!(error, recorded_error("run-missing-unexpected.out", folder));
    prepare(folder, "params.tag = 'b'\n");
    let error = run(folder).expect("the run fails");
    assert_eq!(error, recorded_error("run-changed.out", folder));
    let kept = std::fs::read_to_string(folder.join("tests/snapshot.json")).expect("kept");
    assert_eq!(kept, edited, "a failed check keeps the snapshot");
}

#[test]
fn needs_snapshot_file() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    common::copy(&common::fixtures("nf_snapshot"), folder, &["main.nf"]);
    let config = "plugins { id 'nf-snapshot@0.3.0' }\nsnapshot.enabled = true\n";
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let error = run(folder).unwrap_or_default();
    let message = "nf-snapshot: `snapshot.file` must be set when `snapshot.enabled = true`";
    assert!(error.contains(message), "{error}");
}
