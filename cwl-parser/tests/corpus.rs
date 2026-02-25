//! Checks the loader against `cwltool --validate` on the CWL corpus from `just cwl-corpus`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cwl_parser::source::FileSystem;
use cwl_parser::{Loader, Node, Value, yaml};

const REJECTED: &str = include_str!("fixtures/corpus/rejected.txt");
const DIFFERENCES: &str = include_str!("fixtures/corpus/differences.txt");
const CLASSES: [&str; 4] = ["CommandLineTool", "ExpressionTool", "Workflow", "Operation"];
const SUITES: [&str; 3] = [
    "conformance/v1.0/v1.0/conformance_test_v1.0.yaml",
    "conformance/v1.1/conformance_tests.yaml",
    "conformance/v1.2/conformance_tests.yaml",
];

#[test]
#[ignore = "needs the CWL corpus from `just cwl-corpus`; run it with `just cwl-corpus-test`"]
fn matches_cwltool_validate() {
    let root = PathBuf::from(std::env::var("CWL_CORPUS").expect("CWL_CORPUS is set"));
    let rejected = entries(REJECTED);
    let differences = entries(DIFFERENCES);
    let mut documents = BTreeSet::new();
    collect(&root, &root, &mut documents);
    for suite in SUITES {
        tools(&root, Path::new(suite), &mut documents);
    }
    // A partial download would otherwise pass with fewer documents to check.
    let count = documents.len();
    assert!(count > 6_200, "incomplete corpus: {count} documents");
    let mut mismatches = Vec::new();
    let mut stale = Vec::new();
    let mut agreed = 0;
    for document in &documents {
        let path = root.join(document);
        let loaded = Loader::new(FileSystem).load_entries(&path.to_string_lossy());
        let expected = !rejected.contains_key(document.as_str());
        let agrees = loaded.is_ok() == expected;
        agreed += usize::from(agrees);
        match (agrees, differences.contains_key(document.as_str())) {
            (true, true) => stale.push(document.clone()),
            (false, false) => match loaded {
                Ok(_) => mismatches.push(format!("{document}: loads, but cwltool rejects it")),
                Err(error) => mismatches.push(format!("{document}: {error}")),
            },
            _ => {}
        }
    }
    println!(
        "{count} documents, {agreed} agree with cwltool ({:.1}%), {} known differences",
        agreed as f64 * 100.0 / count as f64,
        count - agreed - mismatches.len()
    );
    assert!(
        stale.is_empty(),
        "differences that now agree:\n{}",
        stale.join("\n")
    );
    let failures = mismatches.len();
    assert!(
        failures == 0,
        "{failures} documents differ:\n{}",
        mismatches.join("\n")
    );
}

/// The first word of each line that is not a comment, with the rest of the line.
fn entries(text: &str) -> BTreeMap<&str, &str> {
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.split_once(' ').unwrap_or((line, "")))
        .collect()
}

/// CWL documents below a folder: files that hold a process, and `.cwl` files that do not parse.
fn collect(root: &Path, directory: &Path, documents: &mut BTreeSet<String>) {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(directory)
        .expect("corpus directory is readable")
        .map(|entry| entry.expect("corpus entry is readable").path())
        .collect();
    paths.sort();
    for path in paths {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        if path.is_dir() {
            if !matches!(name.as_str(), ".git" | "results" | "scripts") {
                collect(root, &path, documents);
            }
            continue;
        }
        if ![".cwl", ".yml", ".yaml", ".json"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        match yaml::read(&text, &Arc::from(relative.as_str())) {
            Ok(node) if holds_process(&node) => {
                documents.insert(relative);
            }
            Err(_) if name.ends_with(".cwl") => {
                documents.insert(relative);
            }
            _ => {}
        }
    }
}

fn holds_process(node: &Node) -> bool {
    let process = |node: &Node| {
        let class = node.get("class").and_then(Node::as_str).unwrap_or("");
        CLASSES.contains(&class.rsplit(':').next().unwrap_or(class))
    };
    match &node.value {
        Value::Map(_) => match node.get("$graph") {
            Some(graph) => graph
                .as_list()
                .is_some_and(|items| items.iter().any(process)),
            None => process(node) || node.get("cwlVersion").is_some(),
        },
        Value::List(items) => !items.is_empty() && items.iter().all(process),
        _ => false,
    }
}

/// The `tool` of each conformance test, with its fragment, following `$import`.
fn tools(root: &Path, suite: &Path, documents: &mut BTreeSet<String>) {
    let text = std::fs::read_to_string(root.join(suite)).expect("the test list is readable");
    let tests = yaml::read(&text, &Arc::from("tests")).expect("the test list parses");
    let folder = suite.parent().unwrap_or(Path::new(""));
    for test in tests.as_list().unwrap_or_default() {
        if let Some(imported) = test.get("$import").and_then(Node::as_str) {
            tools(root, &folder.join(imported), documents);
        } else if let Some(tool) = test.get("tool").and_then(Node::as_str) {
            let (file, fragment) = tool.split_once('#').unwrap_or((tool, ""));
            let path = normalize(&folder.join(file));
            let path = path.to_string_lossy();
            match fragment {
                "" => documents.insert(path.into_owned()),
                fragment => documents.insert(format!("{path}#{fragment}")),
            };
        }
    }
}

/// Removes `..` segments, as Python's `os.path.normpath` does for the oracle.
fn normalize(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::ParentDir => {
                normal.pop();
            }
            std::path::Component::CurDir => {}
            other => normal.push(other),
        }
    }
    normal
}
