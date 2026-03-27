//! Runs every test of the CWL conformance suites v1.0, v1.1 and v1.2 through the driver and
//! the mock TES executor, and compares the outcome with `tests/fixtures/conformance.txt`.
//!
//! The mock runs no tool, so a test checks its expected output only where the output does
//! not depend on a real run: runs of expression tools, parameter references and command
//! lines. For that the executor here simulates the few commands whose effect is known, like
//! `echo`, `cat` of staged files, and the conformance suite's `args.py`, which writes its
//! arguments to `cwl.output.json`; so the command lines are compared with cwltool's. Each test
//! gets one of these outcomes:
//!
//! - `pass`: the output equals the expected one, as cwltest compares it; for `should_fail`
//!   tests, the run fails.
//! - `ran`: the graph was built, but the output needs a real run of some tool.
//! - `fail: ...` and `unsupported: ...`: the run failed, or a checked output differs.
//!
//! Run with `just cwl-conformance-test`; on a difference the test writes the new fixture to
//! the target directory and says where.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use cwl_runtime::{CwlDriver, Options, Value};
use workflow_graph::{ExecuteError, Executor, Launch, MockTes, Outcome, Task, execute};

const SUITES: [(&str, &str); 3] = [
    ("v1.0", "conformance/v1.0/v1.0/conformance_test_v1.0.yaml"),
    ("v1.1", "conformance/v1.1/conformance_tests.yaml"),
    ("v1.2", "conformance/v1.2/conformance_tests.yaml"),
];

struct Test {
    id: String,
    tool: String,
    job: Option<PathBuf>,
    output: Value,
    should_fail: bool,
}
/// Links every file below `from` into the same place below `into`, making folders.
fn link_tree(from: &Path, into: &Path) {
    let Ok(entries) = std::fs::read_dir(from) else {
        return;
    };
    std::fs::create_dir_all(into).expect("creates a folder");
    for entry in entries.flatten() {
        let path = entry.path();
        let target = into.join(entry.file_name());
        let kind = entry.file_type().expect("a file type");
        if kind.is_dir() {
            if entry.file_name() != ".git" {
                link_tree(&path, &target);
            }
        } else if kind.is_symlink() {
            let link = std::fs::read_link(&path).expect("reads a link");
            let _ = std::os::unix::fs::symlink(link, &target);
        } else {
            let _ = std::os::unix::fs::symlink(&path, &target);
        }
    }
}

/// The outputs with every file where cwltool's final output folder would put it: named by
/// its basename, with listings below their directory.
fn relocate(outputs: cwl_runtime::Map) -> Value {
    fn place(value: &mut Value, folder: &str) {
        match value {
            Value::List(items) => items.iter_mut().for_each(|item| place(item, folder)),
            Value::Map(map)
                if matches!(
                    map.get("class").and_then(Value::as_str),
                    Some("File" | "Directory")
                ) =>
            {
                let name = map
                    .get("basename")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let original = map
                    .get("location")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let access = cwl_runtime::files::Access::default();
                if map.get("class").and_then(Value::as_str) == Some("Directory") {
                    access.load_listing(map, true);
                } else if !map.contains_key("checksum") && original.starts_with("file://") {
                    let _ = access.add_checksum(map);
                }
                let location = format!("{folder}/{name}");
                if let Some(inner) = map.get_mut("secondaryFiles") {
                    place(inner, folder);
                }
                if let Some(inner) = map.get_mut("listing") {
                    place(inner, &location);
                }
                map.insert("location".into(), Value::String(location));
            }
            Value::Map(map) => map.iter_mut().for_each(|(_, item)| place(item, folder)),
            _ => {}
        }
    }
    let mut value = Value::Map(outputs);
    place(&mut value, "file:///outputs");
    value
}

/// The first line of a message without the paths that differ between machines.
fn clean(message: &str, tool: &str) -> String {
    let first = message.lines().next().unwrap_or_default();
    let folder = Path::new(tool)
        .parent()
        .map(|folder| folder.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut text = first
        .replace(&format!("file://{folder}/"), "")
        .replace(&format!("{folder}/"), "");
    if let Some(root) = std::env::var_os("CWL_CORPUS") {
        text = text.replace(&root.to_string_lossy().into_owned(), "<corpus>");
    }
    let temporary = std::env::temp_dir().to_string_lossy().into_owned();
    if let Some(start) = text.find(&temporary) {
        let end = text[start..]
            .find([' ', '`', '"', ','])
            .map_or(text.len(), |end| start + end);
        text.replace_range(start..end, "<tmp>");
    }
    text.chars().take(200).collect()
}

/// cwltest's `compare`: expected keys must match, `Any` matches anything, locations match by
/// their end, and directory listings by any order.
fn compare(expected: &Value, actual: &Value) -> Result<(), String> {
    if expected.as_str() == Some("Any") {
        return Ok(());
    }
    if !expected.is_null() && actual.is_null() {
        return Err(format!("expected {}, got null", expected.to_json()));
    }
    match expected {
        Value::Map(map) => {
            let Value::Map(actual_map) = actual else {
                return Err(format!(
                    "expected {}, got {}",
                    expected.to_json(),
                    actual.to_json()
                ));
            };
            match expected.class() {
                Some("File") => compare_file(map, actual_map, actual),
                Some("Directory") => {
                    if actual.class() != Some("Directory") {
                        return Err(format!("expected a Directory, got {}", actual.to_json()));
                    }
                    let Some(Value::List(listing)) = actual_map.get("listing") else {
                        return Err("the Directory has no listing".into());
                    };
                    for item in map
                        .get("listing")
                        .and_then(Value::as_list)
                        .unwrap_or_default()
                    {
                        if !listing.iter().any(|other| compare(item, other).is_ok()) {
                            return Err(format!("{} is not in the listing", item.to_json()));
                        }
                    }
                    compare_file(map, actual_map, actual)
                }
                _ => {
                    for (key, value) in map {
                        compare(value, actual_map.get(key).unwrap_or(&Value::Null))
                            .map_err(|error| format!("{key}: {error}"))?;
                    }
                    for (key, value) in actual_map {
                        if !map.contains_key(key) && !value.is_null() {
                            return Err(format!("unexpected key {key}"));
                        }
                    }
                    Ok(())
                }
            }
        }
        Value::List(items) => {
            let Value::List(actual_items) = actual else {
                return Err(format!("expected a list, got {}", actual.to_json()));
            };
            if items.len() != actual_items.len() {
                return Err(format!(
                    "expected {} items, got {}: {}",
                    items.len(),
                    actual_items.len(),
                    actual.to_json()
                ));
            }
            for (index, (item, other)) in items.iter().zip(actual_items).enumerate() {
                compare(item, other).map_err(|error| format!("[{index}]: {error}"))?;
            }
            Ok(())
        }
        _ if cwl_runtime::value::python_equal(expected, actual) => Ok(()),
        _ => Err(format!(
            "expected {}, got {}",
            expected.to_json(),
            actual.to_json()
        )),
    }
}

fn compare_file(
    expected: &cwl_runtime::Map,
    actual: &cwl_runtime::Map,
    whole: &Value,
) -> Result<(), String> {
    let field = if expected.contains_key("path") {
        Some("path")
    } else if expected.contains_key("location") {
        Some("location")
    } else {
        None
    };
    if let Some(field) = field {
        let wanted = expected
            .get(field)
            .and_then(Value::as_str)
            .unwrap_or_default();
        let got = actual
            .get(field)
            .or(actual.get("location"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim_end_matches('/');
        if wanted != "Any"
            && !(got.ends_with(&format!("/{wanted}")) || (!got.contains('/') && got == wanted))
        {
            return Err(format!("{got} does not end with {wanted}"));
        }
    }
    if let Some(contents) = expected.get("contents") {
        let text = std::fs::read_to_string(
            cwl_runtime::files::local_path(
                actual
                    .get("location")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            )
            .unwrap_or_default(),
        )
        .ok()
        .or_else(|| {
            actual
                .get("contents")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        if text.as_deref() != contents.as_str() {
            return Err(format!("the contents differ: {}", whole.to_json()));
        }
    }
    if actual.get("class").and_then(Value::as_str) == Some("File") {
        for (key, value) in expected {
            if matches!(key.as_str(), "path" | "location" | "listing" | "contents") {
                continue;
            }
            compare(value, actual.get(key).unwrap_or(&Value::Null))
                .map_err(|error| format!("{key}: {error}"))?;
        }
    }
    Ok(())
}
/// Copies a folder with everything in it.
fn copy_tree(from: &Path, into: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(into)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = into.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

#[derive(Default)]
struct Redirects {
    stdin: Option<String>,
    stdout: Option<String>,
    stderr: Option<String>,
}

/// The words of a simple command, and its redirections; `None` for scripts with pipes,
/// substitutions or several commands.
fn command_words(task: &Task) -> Option<(Vec<String>, Redirects)> {
    let [shell, flag, script] = task.command.as_slice() else {
        return Some((task.command.clone(), Redirects::default()));
    };
    if shell != "/bin/sh" || flag != "-c" {
        return Some((task.command.clone(), Redirects::default()));
    }
    let mut redirects = Redirects::default();
    let mut lines = script.lines().collect::<Vec<_>>();
    if let Some(first) = lines.first()
        && let Some(rest) = first.strip_prefix("exec ")
    {
        let words = split(rest)?;
        let mut words = words.iter();
        while let Some(word) = words.next() {
            match word.as_str() {
                ">" => redirects.stdout = words.next().cloned(),
                "<" => redirects.stdin = words.next().cloned(),
                "2>" => redirects.stderr = words.next().cloned(),
                _ => return None,
            }
        }
        lines.remove(0);
    }
    if lines.len() != 1 {
        return None;
    }
    let words = split(lines[0])?;
    Some((words, redirects))
}

/// Splits a line into shell words with quotes; `None` for shell syntax beyond words.
fn split(line: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut characters = line.chars();
    while let Some(character) = characters.next() {
        match character {
            '\'' => {
                in_word = true;
                for inner in characters.by_ref() {
                    if inner == '\'' {
                        break;
                    }
                    word.push(inner);
                }
            }
            '"' => {
                in_word = true;
                while let Some(inner) = characters.next() {
                    match inner {
                        '"' => break,
                        '\\' => word.extend(characters.next()),
                        '$' | '`' => return None,
                        other => word.push(other),
                    }
                }
            }
            '\\' => {
                in_word = true;
                word.extend(characters.next());
            }
            ' ' | '\t' => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '|' | '&' | ';' | '$' | '`' | '(' | ')' | '*' | '?' | '<' | '>' => {
                // Redirections of the exec line stand alone as words.
                if in_word || !matches!(character, '<' | '>') {
                    return None;
                }
                word.push(character);
                in_word = true;
            }
            other => {
                in_word = true;
                word.push(other);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    Some(words)
}
