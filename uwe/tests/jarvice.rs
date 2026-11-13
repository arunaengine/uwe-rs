//! Runs small Nextflow workflows through a local fake of the JARVICE API, which runs each job
//! script here: on the vault, and through a staging root with the uwe binary as the helper.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;
#[path = "../../executors/jarvice/tests/common/mod.rs"]
mod fake;

use std::path::Path;

use fake::FakeJarvice;
use serde_json::json;
use sha2::Digest;

/// The workflow of the nf-jarvice fixture, with an image that needs no registry.
const SCRIPT: &str = "process BIG {
    machineType 'n1'
    output: path 'out.txt'
    script: \"echo big > out.txt\"
}
process SMALL {
    input: path x
    output: stdout
    script: \"cat $x\"
}
workflow { SMALL(BIG()) | view }
";

const IMAGE: &str =
    "ubuntu@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// Runs `uwe run main.nf` with `options` in `folder`, whose `nextflow.config` is `config` and
/// whose `uwe.toml` is `settings`, and gives the exit code and the output.
fn run(folder: &Path, config: &str, settings: &str, options: &[&str]) -> (i32, String) {
    std::fs::write(folder.join("main.nf"), SCRIPT).expect("writes the script");
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let file = folder.join("uwe.toml");
    std::fs::write(&file, settings).expect("writes the settings");
    let output = common::command()
        .args(["run", "main.nf", "-uwe-config"])
        .arg(&file)
        .args(options)
        .current_dir(folder)
        .env("XDG_CACHE_HOME", folder.join("cache"))
        .output()
        .expect("runs uwe");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.code().unwrap_or(-1), text)
}

/// nf-jarvice's config of the fixture, with the address of the fake.
fn pipeline(fake: &FakeJarvice) -> String {
    format!(
        "plugins {{ id 'nf-jarvice@0.7.0' }}\nprocess {{\n executor = 'jarvice'\n \
         container = '{IMAGE}'\n apiUrl = '{}'\n machineType = 'n0'\n vault {{\n  \
         name = 'persistent'\n  readonly = false\n  force = false\n }}\n user {{\n  \
         username = 'probeuser'\n  apikey = 'fake-key-for-local-probe'\n }}\n}}\n",
        fake.address
    )
}

/// uwe's settings for jobs that see `folder` as their vault.
fn vault(folder: &Path) -> String {
    format!(
        "[executor.jarvice]\nvault_path = '{}'\npoll_seconds = 1\n",
        folder.display()
    )
}

/// nf-jarvice's config picks the JARVICE executor: the process settings name the API, the
/// machine type, the vault and the user; uwe's cache is on the vault, which the jobs share.
#[test]
fn runs_jarvice_pipelines() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeJarvice::start();
    let (config, settings) = (pipeline(&fake), vault(folder.path()));
    let (code, output) = run(folder.path(), &config, &settings, &[]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("big"), "{output}");
    let submitted = fake.requests("POST /jarvice/batch");
    assert_eq!(submitted.len(), 2, "{output}");
    let machines: Vec<_> = (submitted.iter())
        .map(|request| request.body["machine"]["type"].clone())
        .collect();
    assert_eq!(machines, [json!("n1"), json!("n0")]);
    for request in &submitted {
        let body = &request.body;
        assert_eq!(
            body["user"],
            json!({"username": "probeuser", "apikey": "<masked>"})
        );
        assert_eq!(body["vault"]["name"], "persistent");
        assert_eq!(body["container"]["image"], format!("library/{IMAGE}"));
    }
    assert!(!output.contains("fake-key-for-local-probe"), "{output}");
}

/// `-executor jarvice` runs a pipeline without an executor of its own through a staging root:
/// the job script fetches the uwe binary with its SHA-256, and no vault is needed.
#[test]
fn runs_staged_jobs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeJarvice::start();
    // The test binary is linked dynamically, so uwe would not upload it; the jobs fetch it.
    let binary = std::fs::read(common::BINARY).expect("reads the binary");
    let sha256: String = (sha2::Sha256::digest(&binary).iter())
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let settings = format!(
        "[executor.jarvice]\napi_url = '{}'\nusername = 'probeuser'\n\
         api_key = 'not-a-real-key'\nmachine_type = 'n0'\npoll_seconds = 1\n\
         staging = 'object-store'\nstaging_root = '{}'\ntask_folder = '{}'\n\
         helper_url = 'file://{}'\nhelper_sha256 = '{sha256}'\n",
        fake.address,
        folder.path().join("staging").display(),
        folder.path().join("node").display(),
        common::BINARY,
    );
    let config = format!("process.container = '{IMAGE}'\n");
    let (code, output) = run(folder.path(), &config, &settings, &["-executor", "jarvice"]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("big"), "{output}");
    let submitted = fake.requests("POST /jarvice/batch");
    assert_eq!(submitted.len(), 2, "{output}");
    assert!(submitted[0].body.get("vault").is_none());
}

/// `-resume` takes the tasks of an earlier run, so the second run submits no job.
#[test]
fn resumes_earlier_runs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeJarvice::start();
    let (config, settings) = (pipeline(&fake), vault(folder.path()));
    for _ in 0..2 {
        let (code, output) = run(folder.path(), &config, &settings, &["-resume"]);
        assert_eq!(code, 0, "{output}");
        assert!(output.contains("big"), "{output}");
    }
    assert_eq!(fake.requests("POST /jarvice/batch").len(), 2);
}
