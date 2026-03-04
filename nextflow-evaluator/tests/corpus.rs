//! Lowers every nf-core module of the corpus from `just corpus`: all must lower, except the
//! files that use features iunctus does not support yet.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use nextflow_evaluator::lower::lower_module;
use nextflow_parser::parse_script;

const UNSUPPORTED: &str = include_str!("fixtures/nf_core_modules_unsupported.txt");

#[test]
#[ignore = "needs the nf-core corpus from `just corpus`; run it with `just corpus-test`"]
fn nf_core_modules_lower() {
    let corpus = PathBuf::from(std::env::var("NF_CORE_CORPUS").expect("NF_CORE_CORPUS is set"));
    let root = corpus.join("modules");
    let expected: BTreeMap<&str, &str> = UNSUPPORTED
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once(' '))
        .collect();
    let mut files = Vec::new();
    collect(&root.join("modules/nf-core"), &mut files);
    // A partial download would otherwise pass with fewer files to check.
    assert!(
        files.len() > 2_000,
        "incomplete corpus: {} files",
        files.len()
    );
    let mut mismatches = Vec::new();
    for path in &files {
        let relative = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        let source = std::fs::read_to_string(path).expect("corpus files are text");
        let parsed = parse_script(&source);
        assert!(parsed.errors.is_empty(), "{relative} parses");
        let first = lower_module(&parsed.tree, &source)
            .err()
            .map(|errors| errors[0].message.clone());
        match (expected.get(relative.as_str()), first) {
            (None, Some(error)) => mismatches.push(format!("{relative}: {error}")),
            (Some(_), None) => mismatches.push(format!("{relative}: lowers now")),
            (Some(expected), Some(error)) if **expected != error => {
                mismatches.push(format!("{relative}: {error}, not {expected}"));
            }
            _ => {}
        }
    }
    let count = mismatches.len();
    assert!(
        count == 0,
        "{count} files differ:\n{}",
        mismatches.join("\n")
    );
}

/// The `main.nf` files below a directory.
fn collect(directory: &Path, files: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(directory).expect("corpus directory is readable");
    for entry in entries {
        let path = entry.expect("corpus entry is readable").path();
        if path.is_dir() {
            collect(&path, files);
        } else if path.file_name().is_some_and(|name| name == "main.nf") {
            files.push(path);
        }
    }
}
