//! Lowers every nf-core module of the test workflows from `just test-workflows`: all must lower, except the
//! files that use features iunctus does not support yet.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use nextflow_evaluator::lower::{Imports, lower_module, lower_script};
use nextflow_evaluator::lowered::DefinitionKind;
use nextflow_parser::ast::Item;
use nextflow_parser::parse_script;

const UNSUPPORTED: &str = include_str!("fixtures/nf_core_modules_unsupported.txt");

#[test]
#[ignore = "needs the nf-core test workflows from `just test-workflows`; run it with `just test-workflows-test`"]
fn nf_core_modules_lower() {
    let test_workflows = PathBuf::from(
        std::env::var("NF_CORE_TEST_WORKFLOWS").expect("NF_CORE_TEST_WORKFLOWS is set"),
    );
    let root = test_workflows.join("modules");
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
        "incomplete test workflows: {} files",
        files.len()
    );
    let mut mismatches = Vec::new();
    for path in &files {
        let relative = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        let source = std::fs::read_to_string(path).expect("test workflows files are text");
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

#[test]
#[ignore = "needs the nf-core test workflows from `just test-workflows`; run it with `just test-workflows-test`"]
fn nf_core_subworkflows_lower() {
    let test_workflows = PathBuf::from(
        std::env::var("NF_CORE_TEST_WORKFLOWS").expect("NF_CORE_TEST_WORKFLOWS is set"),
    );
    let root = test_workflows.join("modules");
    let expected: BTreeMap<&str, &str> = UNSUPPORTED
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once(' '))
        .collect();
    let mut files = Vec::new();
    collect(&root.join("subworkflows/nf-core"), &mut files);
    assert!(
        files.len() > 100,
        "incomplete test workflows: {} files",
        files.len()
    );
    let mut mismatches = Vec::new();
    for path in &files {
        let relative = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        let source = std::fs::read_to_string(path).expect("test workflows files are text");
        let parsed = parse_script(&source);
        assert!(parsed.errors.is_empty(), "{relative} parses");
        let mut files = Files {
            directory: path.parent().unwrap_or(&root).to_owned(),
        };
        let first = lower_script(&parsed.tree, &source, &mut files)
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

/// Includes from files: the source is relative to the including file, and `.nf` is added.
struct Files {
    directory: PathBuf,
}

impl Imports for Files {
    fn kind(&mut self, source: &str, name: &str) -> Result<DefinitionKind, String> {
        if source.starts_with("plugin/") {
            return Err(format!("Plugins are not supported yet: {source}"));
        }
        // `name` means `name.nf`, or `name/main.nf` for a directory.
        let mut path = self.directory.join(source);
        if path.is_dir() {
            path.push("main.nf");
        } else if path.extension().is_none_or(|extension| extension != "nf") {
            path.set_extension("nf");
        }
        let text = std::fs::read_to_string(&path).map_err(|error| format!("{source}: {error}"))?;
        let parsed = parse_script(&text);
        for item in &parsed.tree.items {
            let (defined, kind) = match item {
                Item::Function(function) => (&function.name, DefinitionKind::Function),
                Item::Process(process) => (&process.name, DefinitionKind::Process),
                Item::Workflow(workflow) => match &workflow.name {
                    Some(defined) => (defined, DefinitionKind::Workflow),
                    None => continue,
                },
                _ => continue,
            };
            if defined.text == name {
                return Ok(kind);
            }
        }
        Err(format!(
            "Included name '{name}' is not defined in module '{}'",
            path.display()
        ))
    }
}

/// The `main.nf` files below a directory.
fn collect(directory: &Path, files: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(directory).expect("test workflows directory is readable");
    for entry in entries {
        let path = entry.expect("test workflows entry is readable").path();
        if path.is_dir() {
            collect(&path, files);
        } else if path.file_name().is_some_and(|name| name == "main.nf") {
            files.push(path);
        }
    }
}
