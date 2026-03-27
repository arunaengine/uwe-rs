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

/// The tests of a list, following `$import`; paths are relative to the list that names them.
fn tests(list: &Path) -> Vec<Test> {
    let text = std::fs::read_to_string(list).expect("reads the test list");
    let Value::List(entries) = Value::parse(&text).expect("the test list parses") else {
        panic!("{} is not a list", list.display());
    };
    let folder = list.parent().unwrap_or(Path::new(""));
    let mut tests = Vec::new();
    for entry in entries {
        if let Some(imported) = entry.text("$import") {
            tests.extend(self::tests(&folder.join(imported)));
            continue;
        }
        let Some(tool) = entry.text("tool") else {
            continue;
        };
        let id = entry
            .text("id")
            .or(entry.text("label"))
            .unwrap_or("unnamed")
            .to_owned();
        tests.push(Test {
            id,
            tool: folder.join(tool).to_string_lossy().into_owned(),
            job: entry.text("job").map(|job| folder.join(job)),
            output: entry.get("output").cloned().unwrap_or(Value::Null),
            should_fail: entry.get("should_fail") == Some(&Value::Bool(true)),
        });
    }
    tests
}

/// The outcome of one test, as its fixture line says it.
fn outcome(test: &Test) -> String {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let launch = Launch {
        target: PathBuf::from(&test.tool),
        inputs: test.job.clone(),
        scratch: directory.path().to_path_buf(),
        ..Launch::default()
    };
    let mut executor = Simulator {
        mock: MockTes::new(directory.path().join("mock")),
        unsimulated: 0,
    };
    let result = CwlDriver::new(&launch, Options::default()).and_then(|mut driver| {
        let execution = execute(&test.tool, &mut driver, &mut executor);
        match execution.error {
            Some(ExecuteError::Driver(error)) => Err(error),
            Some(other) => Err(workflow_graph::DriverError::new(other.to_string())),
            None => Ok(driver.outputs().cloned().unwrap_or_default()),
        }
    });
    let simulated = executor.unsimulated == 0;
    if std::env::var("CWL_TEST").is_ok() {
        match &result {
            Ok(outputs) => println!("outputs: {}", Value::Map(outputs.clone()).to_json()),
            Err(error) => println!("error: {}", error.message),
        }
        println!("simulated: {simulated}");
    }
    match (result, test.should_fail) {
        (Err(_), true) => "pass".into(),
        (Ok(_), true) if simulated => "fail: runs, but should fail".into(),
        (Ok(_), true) => "ran".into(),
        (Err(error), false) => {
            let kind = if error.unsupported {
                "unsupported"
            } else {
                "fail"
            };
            format!("{kind}: {}", clean(&error.message, &test.tool))
        }
        (Ok(outputs), false) => match compare(&test.output, &relocate(outputs)) {
            Ok(()) => "pass".into(),
            Err(_) if !simulated => "ran".into(),
            Err(difference) => format!("fail: {}", clean(&difference, &test.tool)),
        },
    }
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

/// The mock TES executor, plus the effect of commands whose effect is known.
struct Simulator {
    mock: MockTes,
    unsimulated: usize,
}

impl Executor for Simulator {
    fn execute(&mut self, task: &Task) -> Result<Outcome, ExecuteError> {
        let mut outcome = self.mock.execute(task)?;
        let work = self.mock.work_directory(task);
        // Inputs in the working directory, like InitialWorkDir entries, are there before the
        // command runs, so outputs that name them hold their content.
        let mut staged = Vec::new();
        for input in &task.inputs {
            if input.path.starts_with('/') {
                continue;
            }
            let target = work.join(&input.path);
            if let Some(parent) = target.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            staged.push(input.path.clone());
            let _ = match (&input.content, input.directory) {
                (Some(content), _) => std::fs::write(&target, content),
                (None, true) => copy_tree(Path::new(&input.source), &target),
                (None, false) => std::fs::copy(&input.source, &target).map(|_| ()),
            };
        }
        match simulate(task, &work) {
            // A known command makes only its own files, not every output the mock made.
            Some(made) => {
                outcome.files.retain(|file| {
                    made.contains(&file.path)
                        || staged.iter().any(|path| {
                            file.path == *path || file.path.starts_with(&format!("{path}/"))
                        })
                });
                for path in made {
                    if !outcome.files.iter().any(|file| file.path == path) {
                        outcome.files.push(workflow_graph::OutputFile {
                            location: work.join(&path),
                            directory: false,
                            path,
                        });
                    }
                }
            }
            None => self.unsimulated += 1,
        }
        Ok(outcome)
    }
}

/// Writes what a known command would write, and gives the paths it wrote; `None` when the
/// command is not known.
fn simulate(task: &Task, work: &Path) -> Option<Vec<String>> {
    let mut made = Vec::new();
    let (words, redirects) = command_words(task)?;
    let read_input = |path: &str| -> Option<String> {
        let input = task.inputs.iter().find(|input| {
            let inside = if input.path.starts_with('/') {
                input.path.clone()
            } else {
                format!("{}/{}", task.workdir.as_deref().unwrap_or(""), input.path)
            };
            inside == path || input.path == path
        })?;
        match &input.content {
            Some(content) => Some(content.clone()),
            None => std::fs::read_to_string(&input.source).ok(),
        }
    };
    let stdin = redirects.stdin.as_deref().map(read_input);
    let output = match words.first().map(String::as_str) {
        Some("echo") => {
            let (newline, rest) = match words.get(1).map(String::as_str) {
                Some("-n") => (false, &words[2..]),
                _ => (true, &words[1..]),
            };
            let mut text = rest.join(" ");
            if newline {
                text.push('\n');
            }
            Some(text)
        }
        Some("cat") => {
            let mut text = String::new();
            let files: Vec<&String> = words[1..]
                .iter()
                .filter(|word| !word.starts_with('-'))
                .collect();
            if files.is_empty() {
                text.push_str(&stdin.clone().flatten()?);
            }
            for file in files {
                text.push_str(&read_input(file)?);
            }
            Some(text)
        }
        Some("python" | "python3")
            if words
                .get(1)
                .is_some_and(|script| script.ends_with("args.py")) =>
        {
            let args: Vec<Value> = words[2..]
                .iter()
                .map(|arg| Value::string(arg.rsplit('/').next().unwrap_or(arg)))
                .collect();
            let mut map = cwl_runtime::Map::new();
            map.insert("args".into(), Value::List(args));
            std::fs::write(work.join("cwl.output.json"), Value::Map(map).to_json()).ok()?;
            made.push("cwl.output.json".to_owned());
            None
        }
        Some("true") => None,
        Some("touch") => {
            for name in &words[1..] {
                std::fs::write(work.join(name), "").ok()?;
                made.push(name.clone());
            }
            None
        }
        _ => return None,
    };
    if let Some(target) = &redirects.stdout {
        std::fs::write(work.join(target), output.unwrap_or_default()).ok()?;
        made.push(target.clone());
    }
    if let Some(target) = &redirects.stderr {
        std::fs::write(work.join(target), "").ok()?;
        made.push(target.clone());
    }
    Some(made)
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

#[test]
#[ignore = "needs the CWL corpus from `just cwl-corpus`; run it with `just cwl-conformance-test`"]
fn conformance_tests_run_as_recorded() {
    let corpus = PathBuf::from(std::env::var("CWL_CORPUS").expect("CWL_CORPUS is set"));
    // The suites with their data files, from `just cwl-conformance-data`, in one folder.
    let merged = tempfile::tempdir().expect("a temporary directory");
    link_tree(
        &corpus.join("conformance"),
        &merged.path().join("conformance"),
    );
    if let Some(data) = std::env::var_os("CWL_CONFORMANCE_DATA") {
        link_tree(
            &PathBuf::from(data).join("conformance"),
            &merged.path().join("conformance"),
        );
    }
    let root = merged.path();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/conformance.txt");
    let expected = std::fs::read_to_string(&fixture).unwrap_or_default();
    let header: Vec<&str> = expected
        .lines()
        .filter(|line| line.starts_with('#'))
        .collect();
    let expected: Vec<&str> = expected
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect();
    // `CWL_TEST=<id>` runs only that test and shows its outputs.
    let only = std::env::var("CWL_TEST").ok();
    let mut actual = Vec::new();
    for (version, list) in SUITES {
        for test in tests(&root.join(list)) {
            if only.as_ref().is_some_and(|only| *only != test.id) {
                continue;
            }
            actual.push(format!("{version} {} {}", test.id, outcome(&test)));
        }
    }
    if only.is_some() {
        println!("{}", actual.join("\n"));
        return;
    }
    for version in ["v1.0", "v1.1", "v1.2"] {
        let lines: Vec<&String> = actual
            .iter()
            .filter(|line| line.starts_with(&format!("{version} ")))
            .collect();
        let count = |kind: &str| {
            lines
                .iter()
                .filter(|line| {
                    line.split(' ')
                        .nth(2)
                        .is_some_and(|word| word.starts_with(kind))
                })
                .count()
        };
        println!(
            "{version}: {} tests, {} pass, {} ran, {} fail, {} unsupported",
            lines.len(),
            count("pass"),
            count("ran"),
            count("fail"),
            count("unsupported")
        );
    }
    if actual != expected {
        let changed: Vec<&String> = actual
            .iter()
            .filter(|line| !expected.contains(&line.as_str()))
            .collect();
        let written = Path::new(env!("CARGO_TARGET_TMPDIR")).join("conformance.txt");
        let mut text = header.join("\n");
        text.push('\n');
        text.push_str(&actual.join("\n"));
        text.push('\n');
        std::fs::write(&written, text).expect("writes the new fixture");
        panic!(
            "{} tests differ:\n{}\n\nThe new fixture is in {}",
            changed.len(),
            changed
                .iter()
                .map(|line| line.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            written.display()
        );
    }
}
