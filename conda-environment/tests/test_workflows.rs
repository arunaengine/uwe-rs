//! Reads every conda environment file of the Snakemake test workflows, the files in their
//! `envs` folders, and checks their accepted and rejected outcomes. Run it with
//! `cargo test -p conda-environment --test test_workflows -- --ignored --nocapture`; the
//! folder is `SNAKEMAKE_TEST_WORKFLOWS`, else `~/.cache/iunctus/test-workflows/snakemake`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use conda_environment::Environment;

/// The environment files below a folder.
fn environment_files(folder: &Path, found: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(folder)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            environment_files(&path, found)?;
            continue;
        }
        let in_envs = path.parent().is_some_and(|parent| parent.ends_with("envs"));
        let yaml = matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("yaml" | "yml")
        );
        if in_envs && yaml {
            found.push(path);
        }
    }
    Ok(())
}

#[test]
#[ignore = "needs the Snakemake test workflows"]
fn reads_workflow_environments() {
    let folder = std::env::var_os("SNAKEMAKE_TEST_WORKFLOWS")
        .map(PathBuf::from)
        .or_else(|| {
            let home = std::env::var_os("HOME")?;
            Some(PathBuf::from(home).join(".cache/iunctus/test-workflows/snakemake"))
        })
        .expect("a folder of test workflows");
    let mut files = Vec::new();
    environment_files(&folder, &mut files).expect("read the complete environment inventory");
    files.sort();
    let baseline: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/workflow_rejections.json"))
            .expect("valid corpus baseline");
    assert_eq!(
        files.len() as u64,
        baseline["total"].as_u64().unwrap(),
        "complete corpus inventory"
    );
    let expected: BTreeMap<String, String> = baseline["rejected"]
        .as_array()
        .unwrap()
        .iter()
        .map(|failure| {
            (
                failure["path"].as_str().unwrap().to_owned(),
                failure["message"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let (mut read, mut specs, mut pip) = (0, 0, 0);
    let mut failures: BTreeMap<String, String> = BTreeMap::new();
    for file in &files {
        let text = std::fs::read_to_string(file)
            .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
        match Environment::parse(&text) {
            Ok(environment) => {
                read += 1;
                specs += environment.dependencies.len();
                pip += environment.pip.len();
            }
            Err(error) => {
                let message = error.to_string();
                let relative = file.strip_prefix(&folder).expect("corpus-relative path");
                failures.insert(relative.to_string_lossy().replace('\\', "/"), message);
            }
        }
    }
    println!(
        "{} environment files: {read} read, with {specs} match specs and {pip} pip packages",
        files.len()
    );
    assert_eq!(
        failures, expected,
        "exact rejected paths and parser outcomes"
    );
    assert_eq!(
        read,
        baseline["accepted"].as_u64().unwrap(),
        "accepted environments"
    );
}
