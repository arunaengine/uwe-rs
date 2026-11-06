//! Runs a small Nextflow workflow through a local fake of Google Batch, which runs each job here
//! with the uwe binary as its helper and gives tokens as the metadata server of a VM.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;
#[path = "../../google-batch-executor/tests/common/mod.rs"]
mod fake;

use std::path::Path;

use fake::FakeGoogle;

const SCRIPT: &str = "process UPPER {
    container 'ubuntu@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef'
    cpus 2
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
fn run(folder: &Path, fake: &FakeGoogle, config: &str, options: &[&str]) -> (i32, String) {
    std::fs::write(folder.join("main.nf"), SCRIPT).expect("writes the script");
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    std::fs::write(folder.join("input.txt"), "hello google\n").expect("writes the input");
    let settings = format!(
        "[executor.google-batch]\nstaging = 'object-store'\nstaging_root = '{}'\n\
         helper_path = '{}'\ntask_folder = '{}'\npoll_seconds = 1\nbatch_endpoint = '{}'\n\
         logging_endpoint = '{}'\n",
        folder.join("staging").display(),
        common::BINARY,
        folder.join("node").display(),
        fake.address,
        fake.address,
    );
    let file = folder.join("uwe.toml");
    std::fs::write(&file, settings).expect("writes the settings");
    // No credentials file, so the token comes from the fake's metadata server.
    let host = fake.address.trim_start_matches("http://");
    let output = common::command()
        .args(["run", "main.nf", "-uwe-config"])
        .arg(&file)
        .args(options)
        .current_dir(folder)
        .env("XDG_CACHE_HOME", folder.join("cache"))
        .env("CLOUDSDK_CONFIG", folder.join("gcloud"))
        .env("GCE_METADATA_HOST", host)
        .env_remove("GOOGLE_APPLICATION_CREDENTIALS")
        .env_remove("GOOGLE_CLOUD_PROJECT")
        .output()
        .expect("runs uwe");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.code().unwrap_or(-1), text)
}

/// The pipeline's `process.executor = 'google-batch'` picks the Google Batch executor, and the
/// `google` scope names the project and location of the job.
#[test]
fn runs_google_pipelines() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeGoogle::start(&folder.path().join("fake"));
    let config = "process.executor = 'google-batch'\ngoogle.project = 'lab'\n\
                  google.location = 'europe-west4'\ngoogle.batch.spot = true\n";
    let (code, output) = run(folder.path(), &fake, config, &[]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("HELLO GOOGLE"), "{output}");
    let created = fake.requests("POST /v1/projects/lab/locations/europe-west4/jobs?job_id=");
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].authorization, "Bearer vm-token");
    let body = created[0].json();
    let spec = &body["taskGroups"][0]["taskSpec"];
    assert_eq!(spec["computeResource"]["cpuMilli"], 2000);
    let policy = &body["allocationPolicy"]["instances"][0]["policy"];
    assert_eq!(policy["provisioningModel"], "SPOT");
}

/// `-executor google-batch` runs a pipeline without an executor of its own, and a run without a
/// project fails before any job starts.
#[test]
fn runs_chosen_executor() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeGoogle::start(&folder.path().join("fake"));
    let config = "google.project = 'lab'\n";
    let (code, output) = run(folder.path(), &fake, config, &["-executor", "google-batch"]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("HELLO GOOGLE"), "{output}");
    let created = fake.requests("POST /v1/projects/lab/locations/us-central1/jobs?job_id=");
    assert_eq!(created.len(), 1);
    let (code, output) = run(folder.path(), &fake, "", &["-executor", "google-batch"]);
    assert_ne!(code, 0, "{output}");
    assert!(output.contains("needs a project"), "{output}");
    assert_eq!(fake.requests("POST /v1/").len(), 1);
}
