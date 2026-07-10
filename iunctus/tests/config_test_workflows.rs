//! Compares `iunctus config -profile test` with `nextflow config -flat -profile test` for every
//! nf-core pipeline of the test workflows. `just config-oracle` records the Nextflow output first;
//! run with `just config-test-workflows-test`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fancy_regex::Regex;

mod common;

fn directory(variable: &str) -> PathBuf {
    PathBuf::from(std::env::var(variable).unwrap_or_else(|_| panic!("{variable} is not set")))
}

/// The pipelines whose results are expected to differ, with the kind of difference.
fn expected() -> BTreeMap<String, String> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config_test_workflows.txt");
    std::fs::read_to_string(path)
        .expect("reads the fixture")
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .map(|line| {
            let mut parts = line.split_whitespace();
            let name = parts.next().expect("a pipeline").to_owned();
            (name, parts.next().expect("a kind").to_owned())
        })
        .collect()
}

/// The output without what cannot be compared: times of day, the warnings Nextflow mixes into
/// its output, the characters its console cannot show, and closures, which Nextflow shows
/// inside lists as object names only.
fn normalised(text: &str) -> String {
    let time = Regex::new(r"\d{4}-\d{2}-\d{2}_\d{2}-\d{2}-\d{2}").expect("a pattern");
    let object = Regex::new(r"_nf_config_[0-9a-f]+(\$_?\w+)*@[0-9a-f]+").expect("a pattern");
    let closure = Regex::new(r"\{(?:[^{}]|\{(?:[^{}]|\{[^{}]*\})*\})*\}").expect("a pattern");
    let text: String = text
        .lines()
        .filter(|line| !line.starts_with("Downloading plugin") && !line.starts_with("WARN: "))
        .map(|line| format!("{line}\n"))
        .collect();
    let text = time.replace_all(&text, "TIME");
    let text = object.replace_all(&text, "CLOSURE");
    let text = closure.replace_all(&text, "CLOSURE");
    text.chars()
        .map(|c| if c.is_ascii() { c } else { '?' })
        .collect()
}

#[test]
#[ignore = "needs the nf-core test workflows and the recorded oracle; run `just config-test-workflows-test`"]
fn configs_equal_the_oracle() {
    let test_workflows = directory("NF_CORE_TEST_WORKFLOWS").join("pipelines");
    let oracle = directory("IUNCTUS_CONFIG_ORACLE");
    let expected = expected();
    let mut pipelines: Vec<String> = std::fs::read_dir(&test_workflows)
        .expect("reads the test workflows")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    pipelines.sort();
    let mut problems = Vec::new();
    for name in &pipelines {
        let read =
            |extension: &str| std::fs::read_to_string(oracle.join(format!("{name}.{extension}")));
        let Ok(exit) = read("exit") else {
            problems.push(format!("{name}: no oracle output"));
            continue;
        };
        let output = common::command()
            .args(["config", "-profile", "test", "."])
            .current_dir(test_workflows.join(name))
            .output()
            .expect("runs iunctus");
        let mine = String::from_utf8_lossy(&output.stdout);
        let kind = expected.get(name).map(String::as_str);
        let result = match (exit.trim() == "0", output.status.code()) {
            (true, Some(0))
                if normalised(&read("txt").expect("the oracle output")) == normalised(&mine) =>
            {
                "same"
            }
            (true, Some(0)) => "different",
            (true, Some(33)) => "unsupported",
            (false, Some(0)) => "accepted",
            (false, _) => "same",
            (true, _) => "failed",
        };
        if Some(result) != kind && !(result == "same" && kind.is_none()) {
            let error = String::from_utf8_lossy(&output.stderr);
            let first = error.lines().next().unwrap_or_default();
            problems.push(format!(
                "{name}: {result}, expected {}: {first}",
                kind.unwrap_or("same")
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
