//! Checks machine readiness diagnostics without reachable external services.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

mod common;

/// Runs `iunctus check-machine` in `directory` with its own cache and no container engine, and
/// gives the exit code and standard output.
fn check_machine(directory: &Path, arguments: &[&str]) -> (i32, String) {
    let output = common::command()
        .arg("check-machine")
        .args(arguments)
        .env("XDG_CACHE_HOME", directory.join("cache"))
        .env("IUNCTUS_ENGINE", directory.join("docker.sock"))
        .env("PATH", directory)
        .env_remove("IUNCTUS_TES_TOKEN")
        .current_dir(directory)
        .output()
        .expect("runs iunctus");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    (output.status.code().unwrap_or(-1), stdout)
}

/// The line of a part of the machine.
fn line<'a>(stdout: &'a str, part: &str) -> &'a str {
    (stdout.lines())
        .find(|line| line[11.min(line.len())..].starts_with(&format!("{part}: ")))
        .unwrap_or_else(|| panic!("no line of {part}: {stdout}"))
}

#[test]
fn explains_missing_engine() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let (code, stdout) = check_machine(directory.path(), &[]);
    assert_eq!(code, 3, "{stdout}");
    let socket = directory.path().join("docker.sock");
    assert_eq!(
        line(&stdout, "container engine"),
        format!(
            "missing    container engine: no container engine answers at {}; iunctus runs every \
             task in a container, so start the engine of {}",
            socket.display(),
            socket.display()
        )
    );
    assert!(
        line(&stdout, "config file").starts_with("not set up "),
        "{stdout}"
    );
    let store = directory.path().join("cache/iunctus/store");
    let store_line = line(&stdout, "store");
    assert!(
        store_line.starts_with(&format!("ready      store: {}, ", store.display())),
        "{stdout}"
    );
    assert!(store_line.ends_with(" free"), "{stdout}");
    let database = directory.path().join("cache/iunctus/runs.sqlite");
    assert_eq!(
        line(&stdout, "run database"),
        format!("ready      run database: {}, 0 runs", database.display())
    );
    for part in ["executor tes", "executor slurm", "executor kubernetes"] {
        assert!(line(&stdout, part).starts_with("not set up "), "{stdout}");
    }
    assert!(
        stdout.ends_with(
            "1 thing is missing for real runs; the line that starts with `missing` says what to \
             do.\n"
        ),
        "{stdout}"
    );
}

#[test]
fn checks_configured_executors() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let config = "[executor.tes]\nserver = \"http://127.0.0.1:1\"\ntoken = { environment = \
                  \"IUNCTUS_TEST_TES_TOKEN\" }\n\n[executor.slurm]\ncontainer = \"podman\"\n\n\
                  [executor.kubernetes]\nkubeconfig = \"nowhere.yaml\"\n";
    std::fs::write(directory.path().join("lab.toml"), config).expect("writes the config file");
    let (code, stdout) = check_machine(directory.path(), &["-iunctus-config", "lab.toml"]);
    assert_eq!(code, 3, "{stdout}");
    assert_eq!(
        line(&stdout, "config file"),
        "ready      config file: lab.toml; no storage providers"
    );
    assert_eq!(
        line(&stdout, "secret"),
        "missing    secret: lab.toml:3: the environment variable IUNCTUS_TEST_TES_TOKEN, which \
         holds this secret, is not set; set it before a run"
    );
    assert!(
        line(&stdout, "executor tes").starts_with(
            "missing    executor tes: the TES server at http://127.0.0.1:1 does not answer: "
        ),
        "{stdout}"
    );
    assert!(!stdout.contains("(os error"), "{stdout}");
    assert_eq!(
        line(&stdout, "executor slurm"),
        "missing    executor slurm: `sbatch`, `squeue` and `scancel` are not on the PATH; run \
         iunctus on a node of the SLURM cluster, like its login node"
    );
    assert_eq!(
        line(&stdout, "executor kubernetes"),
        "missing    executor kubernetes: the kubeconfig nowhere.yaml does not exist"
    );
}

#[test]
fn configuration_failure_continues() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(
        directory.path().join("lab.toml"),
        "[stroe]\nlocation = \"x\"\n",
    )
    .expect("writes the config file");
    let (code, stdout) = check_machine(directory.path(), &["-iunctus-config", "lab.toml"]);
    assert_eq!(code, 3, "{stdout}");
    let config = line(&stdout, "config file");
    assert!(
        config.starts_with(
            "missing    config file: lab.toml:1: iunctus has no section `stroe`; it knows `store`"
        ),
        "{stdout}"
    );
    assert!(config.ends_with("; fix it"), "{stdout}");
    assert!(
        line(&stdout, "run database").starts_with("ready "),
        "{stdout}"
    );
}

#[test]
fn checks_slurm_runtime() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().expect("a temporary directory");
    let tool = |name: &str| {
        let path = directory.path().join(name);
        std::fs::write(&path, "#!/bin/sh\n").expect("writes a tool");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("makes it executable");
    };
    for name in ["sbatch", "squeue", "sacct", "scancel"] {
        tool(name);
    }
    let (code, stdout) = check_machine(directory.path(), &["-executor", "slurm"]);
    assert_eq!(code, 3, "{stdout}");
    assert_eq!(
        line(&stdout, "container engine"),
        "not set up container engine: none answers on this machine; -executor slurm does not \
         need one, only Snakefiles that import Python packages without a stand-in do (decision \
         19)"
    );
    assert_eq!(
        line(&stdout, "executor slurm"),
        "missing    executor slurm: the SLURM tools are on the PATH, but the container runtime \
         `apptainer` is not; the jobs run each task with it on a node, so install it, or name \
         another with -slurm-container"
    );
    tool("apptainer");
    let (code, stdout) = check_machine(directory.path(), &["-executor", "slurm"]);
    assert_eq!(code, 0, "{stdout}");
    let cache = directory.path().join("cache/iunctus");
    assert_eq!(
        line(&stdout, "executor slurm"),
        format!(
            "ready      executor slurm: sbatch, squeue, sacct and scancel are on the PATH; the \
             jobs run tasks with apptainer; the nodes must see {} and {} at the same paths",
            cache.join("scratch/slurm").display(),
            cache.join("store").display()
        )
    );
}
