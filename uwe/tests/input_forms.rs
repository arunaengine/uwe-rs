//! The inputs command exposes the versioned form description of its configured driver.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::collections::BTreeSet;
use std::process::Command;

use serde_json::{Value, json};

fn command(folder: &std::path::Path, name: &str, format: &str) -> Value {
    let output = common::isolated(Command::new(common::BINARY))
        .current_dir(folder)
        .args([
            name,
            "main.nf",
            "-profile",
            "form",
            "--greeting",
            "cli",
            "-format",
            format,
        ])
        .output()
        .expect("runs uwe");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON output")
}

#[test]
fn preserves_form_envelope() {
    let folder = tempfile::tempdir().expect("a folder");
    std::fs::write(
        folder.path().join("main.nf"),
        "params.greeting = 'script'\nworkflow { }\n",
    )
    .unwrap();
    std::fs::write(
        folder.path().join("nextflow.config"),
        "profiles { form { params.greeting = 'profile'; params.profile_only = 'yes' } }\n",
    )
    .unwrap();

    let form = command(folder.path(), "inputs", "form");
    let schema = command(folder.path(), "inputs", "json");
    let outputs = command(folder.path(), "outputs", "json");
    let keys: BTreeSet<&str> = form
        .as_object()
        .expect("the form envelope")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        BTreeSet::from([
            "language",
            "outputs",
            "schema",
            "validation",
            "values",
            "version",
        ])
    );
    assert_eq!(form["version"], 1);
    assert_eq!(form["language"], "nextflow");
    assert_eq!(form["schema"], schema);
    assert_eq!(form["outputs"], outputs);
    assert_eq!(
        form["values"],
        json!({"greeting": "cli", "profile_only": "yes"})
    );
    assert_eq!(
        form["validation"],
        json!({
            "policy": "relaxed",
            "values": "typed-json",
            "defaults": "annotations",
            "advisory": true,
            "ignored": [],
            "remote_content_checked": false
        })
    );
}
