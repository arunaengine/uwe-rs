//! Checks the parser against `nextflow lint` on the text-only nf-core corpus from `just corpus`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use nextflow_parser::{LineIndex, parse_config, parse_script};

const REJECTED: &str = include_str!("fixtures/nf_core_rejected.txt");

#[test]
#[ignore = "needs the nf-core corpus from `just corpus`; run it with `just corpus-test`"]
fn matches_nextflow_lint() {
    let root = PathBuf::from(std::env::var("NF_CORE_CORPUS").expect("NF_CORE_CORPUS is set"));
    let rejected: BTreeSet<&str> = REJECTED
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    let mut files = Vec::new();
    collect(&root, &mut files);
    // A partial download would otherwise pass with fewer files to check.
    assert!(
        files.len() > 14_000,
        "incomplete corpus: {} files",
        files.len()
    );
    let mut mismatches = Vec::new();
    for path in &files {
        let relative = path.strip_prefix(&root).unwrap_or(path);
        let relative = relative.to_string_lossy().replace('\\', "/");
        let source = std::fs::read_to_string(path).expect("corpus files are text");
        let errors = if relative.ends_with(".config") {
            parse_config(&source).errors
        } else {
            parse_script(&source).errors
        };
        match (rejected.contains(relative.as_str()), errors.first()) {
            (false, Some(error)) => {
                let located = LineIndex::new(&source).locate(error).to_string();
                mismatches.push(format!("{relative}:{located}"));
            }
            (true, None) => mismatches.push(format!("{relative}: accepted, but lint rejects it")),
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

/// Scripts and Nextflow configs; `nf-test.config` files belong to nf-test and use another format.
fn collect(directory: &Path, files: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(directory).expect("corpus directory is readable");
    for entry in entries {
        let path = entry.expect("corpus entry is readable").path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if path.is_dir() {
            collect(&path, files);
        } else if name.ends_with(".nf") || (name.ends_with(".config") && name != "nf-test.config") {
            files.push(path);
        }
    }
}
