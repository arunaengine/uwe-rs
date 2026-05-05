//! Checks the parser against Snakemake 9.27.0's parser.py on the test workflows from `just snakemake-test-workflows`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};

use snakemake_parser::parse;

const REJECTED: &str = include_str!("fixtures/test_workflows_rejected.txt");
const RULES: &str = include_str!("fixtures/test_workflows_rules.txt");

/// The Snakemake files of one workflow folder and the rules found in them.
#[derive(Default)]
struct Folder {
    files: usize,
    rules: usize,
    /// Per file: its path, then `rejected` or one line of kind, name and directives per rule.
    listing: String,
}

#[test]
#[ignore = "needs the Snakemake test workflows from `just snakemake-test-workflows`; run it with `just snakemake-test-workflows-test`"]
fn matches_parser_py() {
    let root = PathBuf::from(
        std::env::var("SNAKEMAKE_TEST_WORKFLOWS").expect("SNAKEMAKE_TEST_WORKFLOWS is set"),
    );
    let rejected: BTreeSet<&str> = records(REJECTED).collect();
    let expected: BTreeMap<&str, &str> = records(RULES)
        .filter_map(|line| line.split_once('\t'))
        .collect();
    let mut files = Vec::new();
    for top in ["catalog", "snakemake-tests"] {
        collect(&root, &root.join(top), &mut files);
    }
    files.sort();
    // A partial download would otherwise pass with fewer files to check.
    assert!(
        files.len() >= 3_826,
        "incomplete test workflows: {} files",
        files.len()
    );
    let mut folders: BTreeMap<String, Folder> = BTreeMap::new();
    let mut differences = Vec::new();
    for path in &files {
        let parsed = std::fs::read_to_string(root.join(path))
            .map_err(|error| error.to_string())
            .and_then(|source| parse(&source).map_err(|error| error.to_string()));
        let folder = folders.entry(folder_name(path)).or_default();
        folder.files += 1;
        folder.listing.push_str(path);
        folder.listing.push('\n');
        match (&parsed, rejected.contains(path.as_str())) {
            (Ok(_), true) => {
                differences.push(format!("{path}: accepted, but parser.py rejects it"))
            }
            (Err(error), false) => differences.push(format!("{path}:{error}")),
            _ => {}
        }
        let Ok(snakefile) = parsed else {
            folder.listing.push_str("rejected\n");
            continue;
        };
        for rule in snakefile.rules() {
            folder.rules += 1;
            let kind = if rule.checkpoint {
                "checkpoint"
            } else {
                "rule"
            };
            let name = rule.name.as_ref().map_or("-", |name| name.text.as_str());
            folder.listing.push_str(kind);
            folder.listing.push(' ');
            folder.listing.push_str(name);
            for directive in &rule.directives {
                folder.listing.push(' ');
                folder.listing.push_str(directive.keyword.text());
            }
            folder.listing.push('\n');
        }
    }
    let files_alike = files.len() - differences.len();
    let mut folders_alike = 0;
    for (name, folder) in &folders {
        let found = format!(
            "{}\t{}\t{:016x}",
            folder.files,
            folder.rules,
            fnv(&folder.listing)
        );
        if expected.get(name.as_str()) == Some(&found.as_str()) {
            folders_alike += 1;
            continue;
        }
        let mut message = format!(
            "{name}: expected {:?}, found {found}, listing:\n",
            expected.get(name.as_str())
        );
        for line in folder.listing.lines() {
            writeln!(message, "    {line}").expect("writing to a string succeeds");
        }
        differences.push(message);
    }
    for name in expected.keys().filter(|name| !folders.contains_key(**name)) {
        differences.push(format!("{name}: missing from the test workflows"));
    }
    println!(
        "{files_alike} of {} files accepted or rejected like parser.py; \
         {folders_alike} of {} folders with the same rules and directives",
        files.len(),
        expected.len()
    );
    let count = differences.len();
    assert!(
        count == 0,
        "{count} differences:\n{}",
        differences.join("\n")
    );
}

/// The lines of a fixture without comments.
fn records(fixture: &str) -> impl Iterator<Item = &str> {
    fixture
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
}

/// Snakemake files below `directory` as paths relative to `root`; symbolic links are skipped.
fn collect(root: &Path, directory: &Path, files: &mut Vec<String>) {
    let entries = std::fs::read_dir(directory).expect("test-workflows directory is readable");
    for entry in entries {
        let entry = entry.expect("test-workflows entry is readable");
        let kind = entry.file_type().expect("test-workflows entry has a type");
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if kind.is_dir() {
            collect(root, &path, files);
        } else if kind.is_file()
            && (name == "Snakefile"
                || name == "snakefile"
                || name.ends_with(".smk")
                || name.ends_with(".snakefile"))
        {
            let relative = path.strip_prefix(root).expect("the file is below the root");
            files.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

/// `catalog/<workflow>` or `snakemake-tests/tests/<test>`.
fn folder_name(path: &str) -> String {
    let depth = if path.starts_with("catalog/") { 2 } else { 3 };
    path.split('/').take(depth).collect::<Vec<_>>().join("/")
}

/// FNV-1a with 64 bits, which the fixture's generator computes the same way.
fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}
