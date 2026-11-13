//! Runs a small Nextflow workflow through a local fake of Azure Batch, which runs each task here
//! with the uwe binary as its helper and checks the Shared Key of every request.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;
#[path = "../../executors/azure-batch/tests/common/mod.rs"]
mod fake;

use std::path::Path;

use fake::{FakeAzure, KEY};

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

/// The environment variables of Azure credentials, which the tests leave out.
const VARIABLES: [&str; 7] = [
    "AZURE_BATCH_ACCOUNT_NAME",
    "AZURE_BATCH_ACCOUNT_KEY",
    "AZURE_CLIENT_ID",
    "AZURE_CLIENT_SECRET",
    "AZURE_TENANT_ID",
    "AZURE_MANAGED_IDENTITY_USER",
    "AZURE_MANAGED_IDENTITY_SYSTEM",
];

/// Runs `uwe run main.nf` with `options` in `folder`, whose `nextflow.config` is `config` and
/// whose uwe settings add `extra`, and gives the exit code and the output.
fn run(folder: &Path, config: &str, extra: &str, options: &[&str]) -> (i32, String) {
    std::fs::write(folder.join("main.nf"), SCRIPT).expect("writes the script");
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    std::fs::write(folder.join("input.txt"), "hello azure\n").expect("writes the input");
    let settings = format!(
        "[executor.azure-batch]\nstaging = 'object-store'\nstaging_root = '{}'\n\
         helper_path = '{}'\ntask_folder = '{}'\npoll_seconds = 1\n{extra}",
        folder.join("staging").display(),
        common::BINARY,
        folder.join("node").display(),
    );
    let file = folder.join("uwe.toml");
    std::fs::write(&file, settings).expect("writes the settings");
    let mut command = common::command();
    command
        .args(["run", "main.nf", "-uwe-config"])
        .arg(&file)
        .args(options)
        .current_dir(folder)
        .env("XDG_CACHE_HOME", folder.join("cache"))
        .env("UWE_TEST_AZURE_KEY", KEY);
    for name in VARIABLES {
        command.env_remove(name);
    }
    let output = command.output().expect("runs uwe");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.code().unwrap_or(-1), text)
}

/// The pipeline's `process.executor = 'azurebatch'` picks the Azure Batch executor; the `azure`
/// scope names the account, its key and endpoint, and the `queue` directive names the pool.
#[test]
fn runs_azure_pipelines() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeAzure::start(&folder.path().join("fake"));
    fake.add_pool("small", 4);
    let config = format!(
        "process.executor = 'azurebatch'\nprocess.queue = 'small'\n\
         azure.batch.accountName = 'lab'\nazure.batch.accountKey = '{KEY}'\n\
         azure.batch.endpoint = '{}'\n",
        fake.address
    );
    let (code, output) = run(folder.path(), &config, "", &[]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("HELLO AZURE"), "{output}");
    let tasks = fake.requests("POST /jobs/");
    assert_eq!(tasks.len(), 1);
    assert!(
        tasks[0]
            .header("authorization")
            .starts_with("SharedKey lab:")
    );
    let task = tasks[0].json();
    assert_eq!(task["requiredSlots"], 2);
    assert_eq!(
        fake.unsigned(),
        Vec::<String>::new(),
        "every signature matches"
    );
    assert_eq!(
        fake.requests("PATCH /jobs/").len(),
        1,
        "the run ends its job"
    );
}

/// `-executor azure-batch` runs a pipeline without an executor of its own, with the key from
/// an environment variable; a run without credentials fails before any task starts.
#[test]
fn runs_chosen_executor() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeAzure::start(&folder.path().join("fake"));
    fake.add_pool("small", 4);
    let account = format!(
        "account = 'lab'\nendpoint = '{}'\npool = 'small'\n",
        fake.address
    );
    let keyed = format!("{account}account_key = {{ environment = 'UWE_TEST_AZURE_KEY' }}\n");
    let (code, output) = run(folder.path(), "", &keyed, &["-executor", "azure-batch"]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("HELLO AZURE"), "{output}");
    assert_eq!(fake.requests("POST /jobs/").len(), 1);
    let (code, output) = run(folder.path(), "", &account, &["-executor", "azure-batch"]);
    assert_ne!(code, 0, "{output}");
    assert!(output.contains("Azure Batch needs credentials"), "{output}");
    assert_eq!(fake.requests("POST /jobs/").len(), 1);
}

/// `-resume` takes the task of an earlier run, so the second run adds no task.
#[test]
fn resumes_earlier_runs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeAzure::start(&folder.path().join("fake"));
    fake.add_pool("small", 4);
    let config = format!(
        "process.executor = 'azurebatch'\nprocess.queue = 'small'\n\
         azure.batch.accountName = 'lab'\nazure.batch.accountKey = '{KEY}'\n\
         azure.batch.endpoint = '{}'\n",
        fake.address
    );
    for _ in 0..2 {
        let (code, output) = run(folder.path(), &config, "", &["-resume"]);
        assert_eq!(code, 0, "{output}");
        assert!(output.contains("HELLO AZURE"), "{output}");
    }
    assert_eq!(fake.requests("POST /jobs/").len(), 1);
}
