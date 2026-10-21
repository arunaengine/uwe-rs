//! Checks simulated execution graphs for validated CWL workflow fixtures.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use cwl_parser::source::Web;
use cwl_parser::{Loader, Node, Value, yaml};
use cwl_runtime::{CwlDriver, Options};
use workflow_graph::{ExecuteError, Launch, Simulator, execute_with};

mod order;
#[path = "../../cwl-parser/tests/records/mod.rs"]
mod records;

const REJECTED: &str = include_str!("../../cwl-parser/tests/fixtures/test-workflows/rejected.txt");
/// Documents that cwltool rejects but the loader reads; like those of `REJECTED`, they have no
/// graph to compare with cwltool's.
const DIFFERENCES: &str =
    include_str!("../../cwl-parser/tests/fixtures/test-workflows/differences.txt");
/// Why the documents that fail here cannot run, and how cwltool fails on them, from
/// `fixtures/graph_evidence.py`.
const EVIDENCE: &str = include_str!("fixtures/graph_evidence.txt");
const CLASSES: [&str; 4] = ["CommandLineTool", "ExpressionTool", "Workflow", "Operation"];
const SUITES: [&str; 3] = [
    "conformance/v1.0/v1.0/conformance_test_v1.0.yaml",
    "conformance/v1.1/conformance_tests.yaml",
    "conformance/v1.2/conformance_tests.yaml",
];
/// A graph that takes longer than this counts as hanging.
const LIMIT: Duration = Duration::from_secs(60);

/// The outcome of one document: `ok` and the task count, or the error.
fn outcome(root: &Path, document: &str) -> String {
    let path = root.join(document).to_string_lossy().into_owned();
    // A packed file without `#main` runs each of its processes.
    let references: Vec<String> = match Loader::new(Web::default()).load_entries(&path) {
        Ok(processes) if processes.len() > 1 => processes
            .iter()
            .map(|process| {
                let fragment = process
                    .id
                    .split_once('#')
                    .map_or("", |(_, fragment)| fragment);
                format!(
                    "{}#{fragment}",
                    path.split_once('#').map_or(path.as_str(), |(file, _)| file)
                )
            })
            .collect(),
        _ => vec![path.clone()],
    };
    let mut tasks = 0;
    for reference in references {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(graph(&reference));
        });
        match receiver.recv_timeout(LIMIT) {
            Ok(Ok(count)) => tasks += count,
            Ok(Err(error)) => return clean(&error, root),
            Err(_) => return "timeout".into(),
        }
    }
    format!("ok {tasks}")
}

/// Builds the graph of one process and gives its task count: with the optional inputs of
/// the process null, or else with placeholders for them too, as either is a valid job; the
/// error is the one of the first.
fn graph(reference: &str) -> Result<usize, String> {
    graph_with(reference, Options::default()).or_else(|error| {
        let filled = Options {
            fill_optional: true,
            ..Options::default()
        };
        graph_with(reference, filled).map_err(|_| error)
    })
}

/// Builds the graph of one process with some options.
fn graph_with(reference: &str, options: Options) -> Result<usize, String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let (target, fragment) = match reference.split_once('#') {
        Some((file, fragment)) => (file, Some(fragment)),
        None => (reference, None),
    };
    let launch = Launch {
        target: PathBuf::from(target),
        targets: fragment
            .map(|fragment| vec![fragment.to_owned()])
            .unwrap_or_default(),
        scratch: directory.path().to_path_buf(),
        ..Launch::default()
    };
    let mut driver = CwlDriver::new(&launch, options).map_err(|error| describe(&error))?;
    let mut executor = Simulator::new(directory.path().join("simulator"));
    let execution = execute_with(reference, &mut driver, &mut executor, &order::run_options());
    if let Some(error) = execution.error {
        return Err(match error {
            ExecuteError::Driver(error) => describe(&error),
            other => format!("fail: {other}"),
        });
    }
    execution
        .graph
        .check()
        .map_err(|error| format!("fail: the graph is not valid: {error}"))?;
    Ok(execution.graph.nodes.len())
}

fn describe(error: &workflow_graph::DriverError) -> String {
    let kind = if error.unsupported {
        "unsupported"
    } else {
        "fail"
    };
    format!("{kind}: {}", error.message)
}

/// The first line of a message, with the test workflows folder and temporary files taken out.
fn clean(message: &str, root: &Path) -> String {
    let first = message.lines().next().unwrap_or_default();
    let mut text = first
        .replace(&format!("file://{}/", root.display()), "")
        .replace(&format!("{}/", root.display()), "");
    let temporary = std::env::temp_dir().to_string_lossy().into_owned();
    while let Some(start) = text.find(&temporary) {
        let end = text[start..]
            .find([' ', '`', '"', ','])
            .map_or(text.len(), |end| start + end);
        text.replace_range(start..end, "<tmp>");
    }
    text.chars().take(240).collect()
}

/// The documents that graph_evidence.txt gives evidence for: its lines that are no comment,
/// reason, command or output.
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
        .expect("test workflows directory is readable")
        .map(|entry| entry.expect("test workflows entry is readable").path())
        .collect();
    paths.sort();
    for path in paths {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        if path.is_dir() {
            if !matches!(name.as_str(), ".git" | "results" | "scripts")
                && !name.starts_with(".fetch")
            {
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
    let Ok(text) = std::fs::read_to_string(root.join(suite)) else {
        return;
    };
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

/// Removes `..` segments, as Python's `os.path.normpath` does.
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

#[test]
#[ignore = "needs the CWL test workflows from `just cwl-test-workflows`; run it with `just cwl-graph-test-workflows-test`"]
fn matches_recorded_graphs() {
    let root =
        PathBuf::from(std::env::var("CWL_TEST_WORKFLOWS").expect("CWL_TEST_WORKFLOWS is set"));
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/graph_test_workflows.txt");
    let text = std::fs::read_to_string(&fixture).unwrap_or_default();
    let header: Vec<&str> = text.lines().filter(|line| line.starts_with('#')).collect();
    let expected = entries(&text);
    let rejected = records::reasons(REJECTED);
    let differences = entries(DIFFERENCES);
    let mut documents = BTreeSet::new();
    collect(&root, &root, &mut documents);
    for suite in SUITES {
        tools(&root, Path::new(suite), &mut documents);
    }
    // `CWL_DOCUMENT=<part of a path>` builds only the documents that contain it.
    let only = std::env::var("CWL_DOCUMENT").ok();
    let mut actual = Vec::new();
    for document in &documents {
        if rejected.contains_key(document.as_str()) || differences.contains_key(document.as_str()) {
            continue;
        }
        if only
            .as_ref()
            .is_some_and(|only| !document.contains(only.as_str()))
        {
            continue;
        }
        actual.push(format!("{document} {}", outcome(&root, document)));
    }
    if only.is_some() {
        println!("{}", actual.join("\n"));
        return;
    }
    let ok = actual.iter().filter(|line| line.contains(" ok ")).count();
    let tasks: usize = actual
        .iter()
        .filter_map(|line| {
            line.rsplit_once(" ok ")
                .and_then(|(_, count)| count.parse::<usize>().ok())
        })
        .sum();
    let missing = expected
        .keys()
        .filter(|document| !documents.contains(**document))
        .count();
    println!(
        "{} documents, {ok} build graphs ({tasks} tasks), {} fail; {missing} of the fixture are not in the test workflows",
        actual.len(),
        actual.len() - ok
    );
    // A document may only fail with a record in graph_evidence.txt, and a record is only kept
    // while its document fails.
    let evidence = records::reasons(EVIDENCE);
    let unexplained: Vec<&String> = actual
        .iter()
        .filter(|line| !line.contains(" ok "))
        .filter(|line| !evidence.contains_key(line.split(' ').next().unwrap_or_default()))
        .collect();
    let built: Vec<&&str> = evidence
        .keys()
        .filter(|document| {
            actual
                .iter()
                .any(|line| line.starts_with(&format!("{document} ok ")))
        })
        .collect();
    let changed: Vec<&String> = actual
        .iter()
        .filter(|line| {
            let (document, rest) = line.split_once(' ').unwrap_or((line, ""));
            expected.get(document) != Some(&rest)
        })
        .collect();
    assert!(
        unexplained.is_empty(),
        "documents fail without a record in graph_evidence.txt of why they cannot run:\n{}",
        unexplained
            .iter()
            .map(|line| line.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(
        built.is_empty(),
        "documents of graph_evidence.txt build now; remove their records:\n{}",
        built
            .iter()
            .map(|document| **document)
            .collect::<Vec<_>>()
            .join("\n")
    );
    if !changed.is_empty() {
        let written = Path::new(env!("CARGO_TARGET_TMPDIR")).join("graph_test_workflows.txt");
        let mut text = header.join("\n");
        text.push('\n');
        text.push_str(&actual.join("\n"));
        text.push('\n');
        std::fs::write(&written, text).expect("writes the new fixture");
        panic!(
            "{} documents differ:\n{}\n\nThe new fixture is in {}",
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

#[test]
fn records_provide_evidence() {
    let reasons = records::reasons(EVIDENCE);
    assert!(reasons.len() > 50, "{} records", reasons.len());
    let block =
        "a.cwl\nb.cwl\n= unsupported (decision 18): it needs X.\n$ cwltool a.cwl\n> error\n";
    let parsed = records::reasons(block);
    assert_eq!(
        parsed.get("b.cwl"),
        Some(&"unsupported (decision 18): it needs X.")
    );
    for broken in ["a.cwl\n= failing: x\n> error\n", "a.cwl\n= invalid: x\n"] {
        assert!(
            std::panic::catch_unwind(|| records::reasons(broken)).is_err(),
            "{broken}"
        );
    }
}
