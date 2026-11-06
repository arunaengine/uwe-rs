//! Runs a small Nextflow workflow through a local fake of AWS Batch, which runs each job here
//! with the uwe binary as its helper.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;
#[path = "../../aws-batch-executor/tests/common/mod.rs"]
mod fake;

use std::path::Path;

use fake::FakeBatch;

const SCRIPT: &str = "process UPPER {
    container 'ubuntu@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef'
    input:
    path text
    output:
    path 'out.txt'
    script:
    \"tr a-z A-Z < ${text} > out.txt\"
}

workflow {
    UPPER(file('input.txt')).view { it.text.trim() }
}
";

/// Runs `uwe run main.nf` with `options` in `folder`, whose `nextflow.config` is `config`, and
/// gives the exit code and the output.
fn run(folder: &Path, fake: &FakeBatch, config: &str, options: &[&str]) -> (i32, String) {
    std::fs::write(folder.join("main.nf"), SCRIPT).expect("writes the script");
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    std::fs::write(folder.join("input.txt"), "hello batch\n").expect("writes the input");
    let settings = format!(
        "[executor.aws-batch]\nregion = 'eu-west-1'\nstaging = 'object-store'\n\
         staging_root = '{}'\nhelper_path = '{}'\ntask_folder = '{}'\npoll_seconds = 1\n",
        folder.join("staging").display(),
        common::BINARY,
        folder.join("node").display(),
    );
    let file = folder.join("uwe.toml");
    std::fs::write(&file, settings).expect("writes the settings");
    let output = common::command()
        .args(["run", "main.nf", "-uwe-config"])
        .arg(&file)
        .args(options)
        .current_dir(folder)
        .env("XDG_CACHE_HOME", folder.join("cache"))
        .env("AWS_ENDPOINT_URL_BATCH", &fake.address)
        .env("AWS_ENDPOINT_URL_CLOUDWATCH_LOGS", &fake.address)
        .env("AWS_ACCESS_KEY_ID", "AKIDEXAMPLE")
        .env("AWS_SECRET_ACCESS_KEY", "secret")
        .env_remove("AWS_PROFILE")
        .output()
        .expect("runs uwe");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.code().unwrap_or(-1), text)
}

/// The pipeline's `process.executor = 'awsbatch'` picks the AWS Batch executor, which runs the
/// task as a job of the queue of the process.
#[test]
fn runs_awsbatch_pipelines() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeBatch::start(&folder.path().join("fake"));
    let config = "process.executor = 'awsbatch'\nprocess.queue = 'probe-queue'\n";
    let (code, output) = run(folder.path(), &fake, config, &[]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("HELLO BATCH"), "{output}");
    let submitted = fake.requests("submitjob");
    assert_eq!(submitted.len(), 1);
    assert_eq!(submitted[0].body["jobQueue"], "probe-queue");
    assert!(!fake.requests("describejobs").is_empty());
}

/// `-executor aws-batch` runs a pipeline without an executor of its own; a pipeline whose
/// processes name several executors is refused before any job starts.
#[test]
fn runs_chosen_executor() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeBatch::start(&folder.path().join("fake"));
    let config = "process.queue = 'probe-queue'\n";
    let (code, output) = run(folder.path(), &fake, config, &["-executor", "aws-batch"]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("HELLO BATCH"), "{output}");
    assert_eq!(fake.requests("submitjob").len(), 1);
    let mixed = "process {\n executor = 'awsbatch'\n queue = 'q'\n withName: UPPER { executor = 'local' }\n}\n";
    let (code, output) = run(folder.path(), &fake, mixed, &[]);
    assert_ne!(code, 0, "{output}");
    assert!(output.contains("several executors"), "{output}");
    assert_eq!(fake.requests("submitjob").len(), 1);
}
