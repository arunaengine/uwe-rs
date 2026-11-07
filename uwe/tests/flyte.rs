//! Runs a small Nextflow workflow through a local fake of Flyte's Connect API, which runs each
//! run here after it downloaded the uwe binary as the helper input.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;
#[path = "../../flyte-executor/tests/common/mod.rs"]
mod fake;

use std::path::Path;

use fake::FakeFlyte;
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

/// The settings of `[executor.flyte]` for a staging root in `folder`: the jobs get the uwe
/// binary, which is linked dynamically in tests, from its path with its SHA-256.
fn settings(folder: &Path, more: &str) -> String {
    let binary = std::fs::read(common::BINARY).expect("reads the binary");
    let sha256: String = (sha2::Sha256::digest(&binary).iter())
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!(
        "[executor.flyte]\npoll_seconds = 1\nstaging = 'object-store'\nstaging_root = '{}'\n\
         task_folder = '{}'\nhelper_url = '{}'\nhelper_sha256 = '{sha256}'\n{more}",
        folder.join("staging").display(),
        folder.join("node").display(),
        common::BINARY,
    )
}

/// Runs `uwe run main.nf` with `options` in `folder`, whose `nextflow.config` is `config` and
/// whose `uwe.toml` is `settings`, with the API key `key`, and gives the exit code and output.
fn run(folder: &Path, config: &str, settings: &str, key: &str, options: &[&str]) -> (i32, String) {
    std::fs::write(folder.join("main.nf"), SCRIPT).expect("writes the script");
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    std::fs::write(folder.join("input.txt"), "hello flyte\n").expect("writes the input");
    let file = folder.join("uwe.toml");
    std::fs::write(&file, settings).expect("writes the settings");
    let mut command = common::command();
    for name in flyte_executor::KEY_VARIABLES {
        command.env_remove(name);
    }
    let output = command
        .args(["run", "main.nf", "-uwe-config"])
        .arg(&file)
        .args(options)
        .current_dir(folder)
        .env("XDG_CACHE_HOME", folder.join("cache"))
        .env("FLYTE_API_KEY", key)
        .env_remove("FLYTE_ADMIN_ENDPOINT")
        .env_remove("FLYTE_INTERNAL_EXECUTION_PROJECT")
        .env_remove("FLYTE_INTERNAL_EXECUTION_DOMAIN")
        .output()
        .expect("runs uwe");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.code().unwrap_or(-1), text)
}

/// The pipeline's `process.executor = 'flyte'` picks the Flyte executor, and the `flyte` scope
/// names the project and domain; the API key gives the endpoint, the org and the token.
#[test]
fn runs_flyte_pipelines() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeFlyte::start(&folder.path().join("fake"));
    let config = "process.executor = 'flyte'\nflyte {\n project = 'flows'\n \
                  domain = 'development'\n}\n";
    let settings = settings(folder.path(), "");
    let key = fake.api_key("lab");
    let (code, output) = run(folder.path(), config, &settings, &key, &[]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("HELLO FLYTE"), "{output}");
    let created = fake.requests("/flyteidl2.workflow.RunService/CreateRun");
    assert_eq!(created.len(), 1, "{output}");
    assert_eq!(created[0].token, "Bearer token-1");
    let body = created[0].json();
    assert_eq!(body["runId"]["org"], "lab");
    assert_eq!(body["runId"]["project"], "flows");
    let container = &body["taskSpec"]["taskTemplate"]["container"];
    assert_eq!(container["resources"]["requests"][0]["value"], "2");
    assert_eq!(container["resources"]["requests"][1]["value"], "3072Mi");
}

/// `-executor flyte` runs a pipeline without an executor of its own with the settings of
/// `[executor.flyte]`; without a project and domain it stops before any run starts.
#[test]
fn runs_chosen_executor() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeFlyte::start(&folder.path().join("fake"));
    let key = fake.api_key("lab");
    let options = ["-executor", "flyte"];
    let missing = settings(folder.path(), "");
    let (code, output) = run(folder.path(), "", &missing, &key, &options);
    assert_ne!(code, 0, "{output}");
    assert!(
        output.contains("missing required settings: project, domain"),
        "{output}"
    );
    assert!(fake.requests("/flyteidl2.workflow").is_empty());
    let named = settings(folder.path(), "project = 'flows'\ndomain = 'production'\n");
    let (code, output) = run(folder.path(), "", &named, &key, &options);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("HELLO FLYTE"), "{output}");
    let created = fake.requests("/flyteidl2.workflow.RunService/CreateRun");
    assert_eq!(created[0].json()["runId"]["domain"], "production");
}
