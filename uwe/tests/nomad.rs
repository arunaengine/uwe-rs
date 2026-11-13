//! Runs a small Nextflow workflow through a local fake of the Nomad HTTP API, which runs each
//! job here: on shared folders, and through a staging root with the uwe binary as the helper.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;
#[path = "../../executors/nomad/tests/common/mod.rs"]
mod fake;

use std::path::Path;

use fake::FakeNomad;
use sha2::Digest;

const SCRIPT: &str = "process UPPER {
    container 'ubuntu@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef'
    cpus 2
    memory '3 GB'
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

/// Runs `uwe run main.nf` with `options` in `folder`, whose `nextflow.config` is `config` and
/// whose `uwe.toml` is `settings`, and gives the exit code and the output.
fn run(folder: &Path, config: &str, settings: &str, options: &[&str]) -> (i32, String) {
    std::fs::write(folder.join("main.nf"), SCRIPT).expect("writes the script");
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    std::fs::write(folder.join("input.txt"), "hello nomad\n").expect("writes the input");
    let file = folder.join("uwe.toml");
    std::fs::write(&file, settings).expect("writes the settings");
    let output = common::command()
        .args(["run", "main.nf", "-uwe-config"])
        .arg(&file)
        .args(options)
        .current_dir(folder)
        .env("XDG_CACHE_HOME", folder.join("cache"))
        .env_remove("NOMAD_ADDR")
        .env_remove("NOMAD_TOKEN")
        .env_remove("NOMAD_NAMESPACE")
        .env_remove("NOMAD_REGION")
        .output()
        .expect("runs uwe");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.code().unwrap_or(-1), text)
}

/// The pipeline's `process.executor = 'nomad'` picks the Nomad executor, and the `nomad` scope
/// names the agent, its token, the namespace and the datacenter; the jobs run on shared folders
/// as the user of uwe, without privileges.
#[test]
fn runs_nomad_pipelines() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeNomad::start(&folder.path().join("fake"));
    let config = format!(
        "process.executor = 'nomad'\nnomad {{\n client {{\n  address = '{}'\n  \
         token = 'not-a-real-token'\n }}\n jobs {{\n  namespace = 'lab'\n  \
         datacenters = ['dc1']\n }}\n}}\n",
        fake.address
    );
    let settings = "[executor.nomad]\npoll_seconds = 1\n";
    let (code, output) = run(folder.path(), &config, settings, &[]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("HELLO NOMAD"), "{output}");
    let registered = fake.requests("POST /v1/jobs?namespace=lab");
    assert_eq!(registered.len(), 1, "{output}");
    assert_eq!(registered[0].token, "not-a-real-token");
    let job = &registered[0].json()["Job"];
    assert_eq!(job["Datacenters"], serde_json::json!(["dc1"]));
    let task = &job["TaskGroups"][0]["Tasks"][0];
    assert_eq!(task["Resources"]["Cores"], 2);
    assert_eq!(task["Resources"]["MemoryMB"], 3072);
    assert_eq!(task["Config"]["privileged"], false);
    assert!(task["User"].as_str().is_some_and(|user| user.contains(':')));
    // A successful job is purged, by default.
    let id = job["ID"].as_str().expect("an id");
    assert!(fake.purged(id));
}

/// `-executor nomad` runs a pipeline without an executor of its own through a staging root:
/// Nomad fetches the uwe binary as an artifact with its SHA-256. A privileged config is refused
/// before any job starts.
#[test]
fn runs_chosen_executor() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeNomad::start(&folder.path().join("fake"));
    // The test binary is linked dynamically, so uwe would not upload it; the jobs fetch it.
    let binary = std::fs::read(common::BINARY).expect("reads the binary");
    let sha256: String = (sha2::Sha256::digest(&binary).iter())
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let settings = format!(
        "[executor.nomad]\naddress = '{}'\npoll_seconds = 1\nstaging = 'object-store'\n\
         staging_root = '{}'\ntask_folder = '{}'\nhelper_url = 'file://{}'\n\
         helper_sha256 = '{sha256}'\n",
        fake.address,
        folder.path().join("staging").display(),
        folder.path().join("node").display(),
        common::BINARY,
    );
    let (code, output) = run(folder.path(), "", &settings, &["-executor", "nomad"]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("HELLO NOMAD"), "{output}");
    let registered = fake.requests("POST /v1/jobs");
    assert_eq!(registered.len(), 1, "{output}");
    let task = &registered[0].json()["Job"]["TaskGroups"][0]["Tasks"][0];
    let checksum = &task["Artifacts"][0]["GetterOptions"]["checksum"];
    assert_eq!(
        checksum.as_str(),
        Some(format!("sha256:{sha256}").as_str()),
        "{task}"
    );
    let refused = "nomad.jobs.privileged = true\n";
    let (code, output) = run(folder.path(), refused, &settings, &["-executor", "nomad"]);
    assert_ne!(code, 0, "{output}");
    assert!(output.contains("never starts privileged"), "{output}");
    assert_eq!(fake.requests("POST /v1/jobs").len(), 1);
}

/// `-resume` takes the task of an earlier run, so the second run registers no job.
#[test]
fn resumes_earlier_runs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeNomad::start(&folder.path().join("fake"));
    let config = format!(
        "process.executor = 'nomad'\nnomad.client.address = '{}'\n",
        fake.address
    );
    let settings = "[executor.nomad]\npoll_seconds = 1\n";
    for _ in 0..2 {
        let (code, output) = run(folder.path(), &config, settings, &["-resume"]);
        assert_eq!(code, 0, "{output}");
        assert!(output.contains("HELLO NOMAD"), "{output}");
    }
    assert_eq!(fake.requests("POST /v1/jobs").len(), 1);
}
