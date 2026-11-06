//! Runs a small Nextflow workflow through a local fake of the Snowflake SQL API, which runs the
//! containers of each job service here, with the uwe binary as the helper image's program.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;
#[path = "../../snowflake-executor/tests/common/mod.rs"]
mod fake;

use std::path::{Path, PathBuf};

use fake::FakeSnowflake;

const SCRIPT: &str = "process UPPER {
    container 'docker.io/library/ubuntu@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef'
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

/// The fixed RSA key of the Snowflake tests.
const KEY: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../snowflake-executor/tests/fixtures/snowflake_key.pem"
);

/// The settings of `[executor.snowflake]` for the fake and a staging root in `folder`, with
/// the helper image whose `/uwe` the fake runs as this uwe binary.
fn settings(folder: &Path, fake: &FakeSnowflake, more: &str) -> String {
    format!(
        "[executor.snowflake]\nhost = '{}'\ndatabase = 'LAB'\nschema = 'JOBS'\n\
         external_access_integrations = ['STAGING']\nhelper_image = '/lab/images/repo/uwe:0.1.0'\n\
         poll_seconds = 1\nstaging = 'object-store'\nstaging_root = '{}'\ntask_folder = '{}'\n{more}",
        fake.address,
        folder.join("staging").display(),
        folder.join("node").display(),
    )
}

/// Runs `uwe run main.nf` with `options` in `folder`, whose `nextflow.config` is `config` and
/// whose `uwe.toml` is `settings`, with `folder` as the home folder, and gives the exit code and
/// the output.
fn run(folder: &Path, config: &str, settings: &str, options: &[&str]) -> (i32, String) {
    std::fs::write(folder.join("main.nf"), SCRIPT).expect("writes the script");
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    std::fs::write(folder.join("input.txt"), "hello snowflake\n").expect("writes the input");
    let file = folder.join("uwe.toml");
    std::fs::write(&file, settings).expect("writes the settings");
    let mut command = common::command();
    for name in [
        "SNOWFLAKE_HOME",
        "SNOWFLAKE_DEFAULT_CONNECTION_NAME",
        "SNOWFLAKE_ACCOUNT",
        "SNOWFLAKE_HOST",
        "SNOWFLAKE_DATABASE",
        "SNOWFLAKE_SCHEMA",
        "SNOWFLAKE_WAREHOUSE",
        "computePool",
    ] {
        command.env_remove(name);
    }
    let output = command
        .args(["run", "main.nf", "-uwe-config"])
        .arg(&file)
        .args(options)
        .current_dir(folder)
        .env("HOME", folder)
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

/// The pipeline's `process.executor = 'snowflake'` picks the Snowflake executor, and the
/// `snowflake` scope names the compute pool and maps the image to a Snowflake repository; uwe
/// signs in with the user's key. A `connections.toml` without a default connection is no error.
#[test]
fn runs_snowflake_pipelines() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeSnowflake::start(
        &folder.path().join("fake"),
        Some(PathBuf::from(common::BINARY)),
    );
    let connections = folder.path().join(".snowflake");
    std::fs::create_dir(&connections).expect("makes the folder");
    let other = "[other]\naccount = 'elsewhere'\n";
    std::fs::write(connections.join("connections.toml"), other).expect("writes the file");
    let config = "process.executor = 'snowflake'\nsnowflake {\n computePool = 'TASKS'\n \
                  registryMappings = 'docker.io:/lab/images/repo'\n}\n";
    let more = format!("account = 'myorg-lab'\nuser = 'uwe'\nprivate_key_file = '{KEY}'\n");
    let settings = settings(folder.path(), &fake, &more);
    let (code, output) = run(folder.path(), config, &settings, &[]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("HELLO SNOWFLAKE"), "{output}");
    let started = fake.statements("EXECUTE JOB SERVICE");
    assert_eq!(started.len(), 1, "{output}");
    let text = &started[0];
    assert!(text.contains("\nIN COMPUTE POOL TASKS\n"), "{text}");
    assert!(
        text.contains("\"image\":\"/lab/images/repo/library/ubuntu@sha256:"),
        "{text}"
    );
    assert!(text.contains("\"memory\":\"3072Mi\""), "{text}");
    let submitted = fake.requests("POST /api/v2/statements?");
    assert!(
        submitted
            .iter()
            .all(|request| request.kind == "KEYPAIR_JWT")
    );
    assert_eq!(fake.statements("DROP SERVICE IF EXISTS UWE_").len(), 1);
}

/// `-executor snowflake` takes the account and an access token from the default connection of
/// `~/.snowflake/connections.toml`; without a compute pool it stops before any job starts.
#[test]
fn runs_chosen_executor() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeSnowflake::start(
        &folder.path().join("fake"),
        Some(PathBuf::from(common::BINARY)),
    );
    let connections = folder.path().join(".snowflake");
    std::fs::create_dir(&connections).expect("makes the folder");
    let connection = format!(
        "[default]\naccount = 'myorg-lab'\nuser = 'uwe'\ntoken = '{}'\n",
        fake::TOKEN
    );
    std::fs::write(connections.join("connections.toml"), connection).expect("writes the file");
    let options = ["-executor", "snowflake"];
    let missing = settings(folder.path(), &fake, "");
    let (code, output) = run(folder.path(), "", &missing, &options);
    assert_ne!(code, 0, "{output}");
    assert!(output.contains("needs a compute pool"), "{output}");
    assert!(fake.requests("POST").is_empty());
    let named = settings(folder.path(), &fake, "compute_pool = 'TASKS'\n");
    let (code, output) = run(folder.path(), "", &named, &options);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("HELLO SNOWFLAKE"), "{output}");
    let submitted = fake.requests("POST /api/v2/statements?");
    assert!(
        submitted
            .iter()
            .all(|request| request.kind == "PROGRAMMATIC_ACCESS_TOKEN")
    );
}
