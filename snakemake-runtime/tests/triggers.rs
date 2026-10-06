//! Rerun triggers select independent signature checks and profile or CLI precedence.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::time::{Duration, SystemTime};

use workflow_graph::{Files, Launch, LocalFiles, Outcome, OutputFile};

const WORKFLOW: &str = "rule make:\n    input: 'in'\n    output: 'out'\n    params: 'before'\n    container: 'before'\n    shell: 'echo {params} > {output}'\n";

fn write(folder: &Path, name: &str, time: u64) {
    let path = folder.join(name);
    std::fs::write(&path, name).unwrap();
    std::fs::File::open(path)
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(time))
        .unwrap();
}

fn reruns(trigger: &str, change: &str, cli: Option<Vec<String>>) -> bool {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path();
    let launch = Launch {
        target: path.into(),
        scratch: path.join("scratch"),
        ..Launch::default()
    };
    let initial = if change == "default-image" {
        WORKFLOW.replace("    container: 'before'\n", "")
    } else {
        WORKFLOW.into()
    };
    std::fs::write(path.join("Snakefile"), &initial).unwrap();
    write(path, "in", 1);
    write(path, "other", 1);
    write(path, "raw", 1);
    let mut driver = snakemake_runtime::driver_with_options(
        &launch,
        Default::default(),
        snakemake_runtime::RunSettings {
            image: (change == "default-image").then(|| "before".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let task = driver.start().unwrap().remove(0);
    driver
        .complete(Outcome {
            id: task.id,
            files: vec![OutputFile {
                path: "out".into(),
                location: path.join("raw").to_string_lossy().into_owned(),
                ..OutputFile::default()
            }],
            ..Outcome::default()
        })
        .unwrap();
    for delivery in driver.deliveries() {
        LocalFiles.deliver(&delivery).unwrap();
        driver.delivered(&delivery).unwrap();
    }
    driver.finish().unwrap();
    write(path, "out", 2);
    let text = match change {
        "params" => WORKFLOW.replace("params: 'before'", "params: 'after'"),
        "input" => WORKFLOW.replace("input: 'in'", "input: 'other'"),
        "software-env" => WORKFLOW.replace("container: 'before'", "container: 'after'"),
        "code" => WORKFLOW.replace("echo {params}", "printf {params}"),
        "mtime" => {
            write(path, "in", 3);
            WORKFLOW.into()
        }
        "default-image" => initial,
        _ => unreachable!(),
    };
    std::fs::write(path.join("Snakefile"), text).unwrap();
    let profile = path.join("profiles/default");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(
        profile.join("config.yaml"),
        format!("rerun-triggers: [{trigger}]\n"),
    )
    .unwrap();
    let mut driver = snakemake_runtime::driver_with_options(
        &launch,
        Default::default(),
        snakemake_runtime::RunSettings {
            rerun_triggers: cli,
            image: (change == "default-image").then(|| "after".into()),
            ..Default::default()
        },
    )
    .unwrap();
    !driver.start().unwrap().is_empty()
}

#[test]
fn selects_signature_checks() {
    for trigger in ["params", "input", "software-env", "code", "mtime"] {
        for change in ["params", "input", "software-env", "code", "mtime"] {
            assert_eq!(
                reruns(trigger, change, None),
                trigger == change,
                "{trigger} and {change}"
            );
        }
    }
}

#[test]
fn cli_overrides_profile() {
    assert!(!reruns("params", "params", Some(vec!["mtime".into()])));
    assert!(reruns("mtime", "params", Some(vec!["params".into()])));
}

#[test]
fn tracks_default_image() {
    assert!(reruns("software-env", "default-image", None));
    assert!(!reruns("code", "default-image", None));
    assert!(!reruns("mtime", "default-image", None));
}

#[test]
fn rejects_unknown_triggers() {
    let folder = tempfile::tempdir().unwrap();
    let profile = folder.path().join("profiles/default");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("config.yaml"), "rerun-triggers: [unknown]\n").unwrap();
    std::fs::write(folder.path().join("Snakefile"), WORKFLOW).unwrap();
    let launch = Launch {
        target: folder.path().into(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let error = snakemake_runtime::driver(&launch)
        .unwrap()
        .start()
        .unwrap_err();
    assert!(
        error.to_string().contains("Invalid rerun trigger"),
        "{error}"
    );
}
