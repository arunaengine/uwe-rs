//! Runs small Nextflow workflows through a local HTTPS fake of a wr manager with its own CA,
//! which runs each job here: on shared folders, and through a staging root with the helper.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;
#[path = "../../wr-executor/tests/common/mod.rs"]
mod fake;

use std::path::{Path, PathBuf};

use fake::FakeWr;
use serde_json::json;

/// Runs the command after the image without a container, as the fake of the scheduler tests:
/// it sees the folders of the binds at their targets by rewriting the paths of the wrapper.
const APPTAINER: &str = r#"#!/bin/bash
binds=()
while [ $# -gt 0 ]; do
    case $1 in
        --pwd) start=$2; shift 2 ;;
        --bind) rest=${2#*:}; binds+=("${rest%%:*}" "${2%%:*}"); shift 2 ;;
        exec|--*) shift ;;
        *) image=$1; shift; break ;;
    esac
done
place() {
    local path=$1 index
    for ((index = 0; index < ${#binds[@]}; index += 2)); do
        case $path in
            "${binds[index]}"|"${binds[index]}"/*)
                printf '%s' "${binds[index + 1]}${path#"${binds[index]}"}"; return ;;
        esac
    done
    printf '%s' "$path"
}
text=$(cat "$(place "$2")")
for ((index = 0; index < ${#binds[@]}; index += 2)); do
    text=${text//"${binds[index]}"/"${binds[index + 1]}"}
done
wrapper=$(mktemp)
printf '%s\n' "$text" > "$wrapper"
cd "$(place "$start")" || exit 255
exec bash "$wrapper"
"#;

const SCRIPT: &str = "process BIG {
    container 'docker://debian:stable-slim'
    cpus 2
    memory '4 GB'
    time '1h'
    output: path 'out.txt'
    script: \"echo big > out.txt\"
}
process SMALL {
    container 'docker://debian:stable-slim'
    input: path x
    output: stdout
    script: \"cat $x\"
}
workflow { SMALL(BIG()) | view }
";

/// Writes the fake `apptainer` into `folder/bin`.
fn tools(folder: &Path) -> PathBuf {
    let bin = folder.join("bin");
    std::fs::create_dir_all(&bin).expect("makes the folder");
    // A child process writes the command, so that no test holds it open for writing.
    let script = "printf '%s' \"$1\" > \"$2\" && chmod 755 \"$2\"";
    let status = std::process::Command::new("sh")
        .args(["-c", script, "sh", APPTAINER])
        .arg(bin.join("apptainer"))
        .status();
    assert!(status.expect("writes the command").success());
    bin
}

/// Runs `uwe run main.nf` with `options` in `folder`, with the fake tools first on the PATH
/// and `home` as the home folder, and gives the exit code and the output.
fn run(folder: &Path, config: &str, settings: &str, options: &[&str]) -> (i32, String) {
    std::fs::write(folder.join("main.nf"), SCRIPT).expect("writes the script");
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let file = folder.join("uwe.toml");
    std::fs::write(&file, settings).expect("writes the settings");
    let path = format!(
        "{}:{}",
        folder.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = common::command()
        .args(["run", "main.nf", "-uwe-config"])
        .arg(&file)
        .args(options)
        .current_dir(folder)
        .env("PATH", path)
        .env("HOME", folder.join("home"))
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

/// nf-wr's config picks the wr executor and names the manager, its token and its CA; the jobs
/// run on shared folders with the resources of their tasks.
#[test]
fn runs_wr_pipelines() {
    let folder = tempfile::tempdir().expect("a folder");
    let bin = tools(folder.path());
    let fake = FakeWr::start(Some(&bin));
    let files = folder.path().join("files");
    fake.write_files(&files, "production");
    let config = format!(
        "plugins {{ id 'nf-wr' }}\nprocess.executor = 'wr'\nexecutor {{\n $wr {{\n  \
         endpoint = '{}'\n  tokenpath = '{}'\n  cacertpath = '{}'\n }}\n}}\n",
        fake.address,
        files.join(".wr_production/client.token").display(),
        files.join(".wr_production/ca.pem").display(),
    );
    let (code, output) = run(folder.path(), &config, "[executor.wr]\n", &[]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("big"), "{output}");
    let added = fake.requests("POST /rest/v1/jobs/");
    assert_eq!(added.len(), 2, "{output}");
    assert!(added.iter().all(|request| request.authorized));
    let big = &added[0].body[0];
    for (field, value) in [
        ("cwd_matters", json!(true)),
        ("override", json!(2)),
        ("cpus", json!(2)),
        ("memory", json!("4096M")),
        ("retries", json!(0)),
    ] {
        assert_eq!(big[field], value, "{field}");
    }
}

/// `-executor wr` runs a pipeline without an executor of its own through a staging root, with
/// the token and CA of the manager in the home folder and the uwe binary as the helper.
#[test]
fn runs_staged_jobs() {
    let folder = tempfile::tempdir().expect("a folder");
    let bin = tools(folder.path());
    let fake = FakeWr::start(Some(&bin));
    fake.write_files(&folder.path().join("home"), "production");
    let settings = format!(
        "[executor.wr]\nendpoint = '{}'\nstaging = 'object-store'\nstaging_root = '{}'\n\
         node_folder = '{}'\nhelper_path = '{}'\n",
        fake.address,
        folder.path().join("staging").display(),
        folder.path().join("node").display(),
        common::BINARY,
    );
    let (code, output) = run(folder.path(), "", &settings, &["-executor", "wr"]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("big"), "{output}");
    let added = fake.requests("POST /rest/v1/jobs/");
    assert_eq!(added.len(), 2, "{output}");
    let job = &added[0].body[0];
    assert_eq!(job["cwd_matters"], false);
    assert_eq!(job["on_exit"], json!([{"cleanup": true}]));
    let node = std::fs::read_dir(folder.path().join("node")).expect("the node folder");
    assert_eq!(node.count(), 0, "the jobs remove their folders");
}
