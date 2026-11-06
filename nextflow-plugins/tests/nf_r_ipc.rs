//! nf-r-ipc against the messages of nf-r-ipc 0.2.2 with Nextflow 26.04.6, and calls through a
//! busybox container whose stand-in for R gives recorded answers, as the recording did.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

const INCLUDE: &str =
    "include { rFunction; rTable; rRecords; channelFromR } from 'plugin/nf-r-ipc'";

/// Runs a workflow body in `folder`, with the work folder there; the error and the printed text.
fn run_in(folder: &Path, body: &str) -> (Option<String>, String) {
    let script = format!("{INCLUDE}\nworkflow {{\n{body}\n}}\n");
    std::fs::write(folder.join("main.nf"), script).expect("writes the script");
    let config = format!(
        "plugins {{ id 'nf-r-ipc@0.2.2' }}\nworkDir = '{}/work'\n",
        folder.display()
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, printed) = common::run(folder, "ripc");
    (execution.error.map(|error| error.to_string()), printed)
}

#[test]
fn checks_calls_first() {
    let cases = [
        (
            "_runtime: 'cloud'",
            "Invalid _runtime value: cloud. Expected 'local', 'conda' or 'docker'",
        ),
        (
            "_docker_image: 'r:1', _docker_container: 'c'",
            "Docker runtime accepts either '_docker_image' or '_docker_container', not both",
        ),
        (
            "_runtime: 'docker'",
            "Docker runtime requires '_docker_image'/'nfR.docker_image' or \
             '_docker_container'/'nfR.docker_container'",
        ),
        (
            "_docker_container: 'c', _docker_mounts: '/a:/b'",
            "The '_docker_mounts' option can only be used with '_docker_image' / docker run",
        ),
        (
            "_runtime: 'conda'",
            "Conda runtime requires '_conda_env' or 'nfR.conda_env'",
        ),
        (
            "_executable: 'R', _conda_env: 'r-base'",
            "The '_executable' and '_conda_env' options cannot be used together",
        ),
        (
            "_runtime: 'local', _conda_env: 'r-base'",
            "The '_conda_env' option requires Conda runtime",
        ),
        (
            "_payload_kind: 'csv'",
            "Invalid _payload_kind value: csv. Expected 'value_graph' or 'table'",
        ),
        (
            "d: new Date(0)",
            "Unsupported value type in value graph: java.util.Date",
        ),
        (
            "_conda_env: 'r-base'",
            "cannot use the conda environment r-base",
        ),
        (
            "_docker_container: 'c'",
            "cannot use the running container c",
        ),
    ];
    let directory = tempfile::tempdir().expect("a temporary directory");
    for (options, expected) in cases {
        let body = format!("println rFunction([function: 'f', {options}], 'f <- function() 1')");
        let (error, printed) = run_in(directory.path(), &body);
        let error = error.unwrap_or_default();
        assert!(error.contains(expected), "{options}: {error}\n{printed}");
    }
    let (error, _) = run_in(directory.path(), "println rFunction([function: 'f'])");
    assert!(
        error
            .unwrap_or_default()
            .contains("Missing script or code argument")
    );
    let body = "println rFunction([function: 'f', script: 'x.R'], 'f <- 1')";
    let (error, _) = run_in(directory.path(), body);
    let error = error.unwrap_or_default();
    assert!(
        error.contains("Cannot use both code and script options together"),
        "{error}"
    );
}

fn busybox() -> bool {
    let found = Command::new("docker")
        .args(["image", "inspect", "--format", "{{.Id}}", "busybox:latest"])
        .output();
    found.is_ok_and(|output| output.status.success())
}

/// A stand-in for R in the work folder: prints a line, then copies an answer or fails.
fn stand_in(folder: &Path, name: &str, answer: Option<&str>) -> String {
    let base = folder.join("work/.nf-r-ipc");
    std::fs::create_dir_all(&base).expect("makes the folder");
    let copy = match answer {
        Some(answer) => {
            let source = common::fixtures("nf_r_ipc").join(answer);
            std::fs::copy(source, base.join(answer)).expect("copies the answer");
            format!("cp \"$(dirname \"$0\")/{answer}\" \"$NFR_RESPONSE_IPC\"\nexit 0\n")
        }
        None => "exit 1\n".to_owned(),
    };
    let script = base.join(name);
    let text = format!("#!/bin/sh\necho \"fake launcher output line\"\n{copy}");
    std::fs::write(&script, text).expect("writes the stand-in");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    script.to_string_lossy().into_owned()
}

#[test]
fn calls_through_container() {
    if !busybox() {
        eprintln!("skipped: Docker or the image busybox:latest is not on this machine");
        return;
    }
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let docker = |program: &str| {
        format!("function: 'f', _docker_image: 'busybox:latest', _container_rscript: '{program}'")
    };
    let types = stand_in(folder, "types.sh", Some("ok_types.arrows"));
    let body = format!(
        "def o = rFunction([{}, sample: 'S1', n: 3], 'f <- function(...) list(...)')\n\
         println 'keys=' + o.keySet()\nprintln 'control=' + o.control\n\
         println 'forwarded=' + o.forwarded_args\nprintln 'decoded=' + o.decoded_data\n\
         println 'output=' + o.launcher_output.trim()",
        docker(&types)
    );
    let (error, printed) = run_in(folder, &body);
    assert!(error.is_none(), "{error:?}\n{printed}");
    for line in [
        "keys=[call_id, codec, runtime, forwarded_args, control, decoded_data, launcher_output]",
        "control=[protocol_version:1, call_id:x, function:f, script_mode:inline, \
         script_ref:<inline>, payload_kind:value_graph, status:ok, result_kind:value_graph]",
        "forwarded=[sample:S1, n:3]",
        "decoded=[s:S1, i:7, d:2.5, b:true, n:null, na_l:LOGICAL, na_i:INTEGER, na_d:DOUBLE, \
         na_c:CHARACTER, lst:[Infinity, NaN], df:[x:[1.0, DOUBLE]]]",
        "output=fake launcher output line",
    ] {
        assert!(
            printed.lines().any(|printed| printed == line),
            "{line}\n{printed}"
        );
    }
    let columns = stand_in(folder, "columns.sh", Some("ok_columns.arrows"));
    let body = format!(
        "println 'rTable=' + rTable([{}], 'f <- 1')\n\
         channelFromR([{}], 'f <- 1').view {{ 'row ' + it }}",
        docker(&columns),
        docker(&columns)
    );
    let (error, printed) = run_in(folder, &body);
    assert!(error.is_none(), "{error:?}\n{printed}");
    assert!(
        printed.contains("rTable=[[sample:A, v:3.0], [sample:B, v:4.0]]"),
        "{printed}"
    );
    assert!(printed.contains("row [sample:B, v:4.0]"), "{printed}");
    let failing = stand_in(folder, "error.sh", Some("error.arrows"));
    let body = format!("println rFunction([{}], 'f <- 1')", docker(&failing));
    let (error, _) = run_in(folder, &body);
    let error = error.unwrap_or_default();
    assert!(error.contains("] RRuntimeError: boom for diagnostics\nLauncher output (tail): fake launcher output line"), "{error}");
    let silent = stand_in(folder, "silent.sh", None);
    let body = format!("println rFunction([{}], 'f <- 1')", docker(&silent));
    let (error, _) = run_in(folder, &body);
    let error = error.unwrap_or_default();
    assert!(
        error.contains("R launcher failed (exit=1)\nfake launcher output line"),
        "{error}"
    );
}
