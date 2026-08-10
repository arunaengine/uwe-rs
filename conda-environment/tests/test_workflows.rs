//! Reads every conda environment file of the Snakemake test workflows, the files in their
//! `envs` folders, and counts what reads. Run it with
//! `cargo test -p conda-environment --test test_workflows -- --ignored --nocapture`; the
//! folder is `SNAKEMAKE_TEST_WORKFLOWS`, else `~/.cache/iunctus/test-workflows/snakemake`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use conda_environment::Environment;

/// The environment files below a folder.
fn environment_files(folder: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            environment_files(&path, found);
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
}

#[test]
#[ignore = "needs the Snakemake test workflows"]
fn the_environments_of_the_snakemake_test_workflows_read() {
    let folder = std::env::var_os("SNAKEMAKE_TEST_WORKFLOWS")
        .map(PathBuf::from)
        .or_else(|| {
            let home = std::env::var_os("HOME")?;
            Some(PathBuf::from(home).join(".cache/iunctus/test-workflows/snakemake"))
        })
        .expect("a folder of test workflows");
    let mut files = Vec::new();
    environment_files(&folder, &mut files);
    files.sort();
    let (mut read, mut specs, mut pip) = (0, 0, 0);
    let mut failures: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for file in &files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        match Environment::parse(&text) {
            Ok(environment) => {
                read += 1;
                specs += environment.dependencies.len();
                pip += environment.pip.len();
            }
            Err(error) => {
                let message = error.to_string();
                let kind = message.split(": ").nth(1).unwrap_or(&message).to_owned();
                let relative = file.strip_prefix(&folder).unwrap_or(file);
                let entry = failures.entry(kind).or_default();
                entry.push(format!("{}: {message}", relative.display()));
            }
        }
    }
    println!(
        "{} environment files: {read} read, with {specs} match specs and {pip} pip packages",
        files.len()
    );
    for (kind, found) in &failures {
        println!("{} fail with {kind}; like {}", found.len(), found[0]);
    }
    assert!(
        !files.is_empty(),
        "no environment files in {}",
        folder.display()
    );
}
