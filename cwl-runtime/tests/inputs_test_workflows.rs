//! Describes the inputs of every CWL document of the graph test (`graph_test_workflows.txt`)
//! as a run does before its first task (decision 24), and compares the counts with
//! `tests/fixtures/inputs_test_workflows.txt`: the inputs of each schema and how many are
//! required, files and directories, the errors and warnings of a check of a simulated run
//! without a job file, and the inputs that the process reads or leaves unread. Every schema
//! must be valid JSON Schema of draft 2020-12. No task runs. A document that loads a `run` file
//! from the web depends on the network, so it is compared only when it loads both times. Run
//! with `just inputs-test-workflows-test`; `UPDATE_FIXTURE=1` writes the fixture again.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use cwl_runtime::{CwlDriver, Options};
use input_schema::Counts;
use workflow_graph::{Driver, Launch};

const DOCUMENTS: &str = include_str!("fixtures/graph_test_workflows.txt");
const FIXTURE: &str = "tests/fixtures/inputs_test_workflows.txt";

/// What the error of a document says when it could not read a file from the web.
const NO_NETWORK: &str = "cannot read `http";

/// The inputs of one document, as the fixture records them.
fn describe(root: &Path, document: &str) -> String {
    let (file, fragment) = match document.split_once('#') {
        Some((file, fragment)) => (file, Some(fragment)),
        None => (document, None),
    };
    let launch = Launch {
        target: root.join(file),
        targets: fragment.map(|f| vec![f.to_owned()]).unwrap_or_default(),
        simulated: true,
        ..Launch::default()
    };
    let described = CwlDriver::new(&launch, Options::default()).and_then(|mut driver| {
        let inputs = driver.inputs()?;
        Ok(inputs.map(|inputs| Counts::of(&inputs, true).to_string()))
    });
    match described {
        Ok(Some(counts)) => counts,
        Ok(None) => "none".into(),
        Err(error) => {
            let kind = if error.unsupported {
                "unsupported"
            } else {
                "fail"
            };
            let first = error.message.lines().next().unwrap_or_default();
            let first = first
                .replace(&format!("file://{}/", root.display()), "")
                .replace(&format!("{}/", root.display()), "");
            format!("{kind}: {}", first.chars().take(200).collect::<String>())
        }
    }
}

#[test]
#[ignore = "needs the CWL test workflows from `just cwl-test-workflows`; run it with `just inputs-test-workflows-test`"]
fn documents_describe_their_inputs() {
    let root =
        PathBuf::from(std::env::var("CWL_TEST_WORKFLOWS").expect("CWL_TEST_WORKFLOWS is set"));
    let documents: Vec<&str> = DOCUMENTS
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split(' ').next())
        .filter(|document| {
            let file = document.split('#').next().unwrap_or_default();
            root.join(file).is_file()
        })
        .collect();
    let next = AtomicUsize::new(0);
    let lines = Mutex::new(vec![String::new(); documents.len()]);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(document) = documents.get(index) else {
                        break;
                    };
                    let line = format!("{document} {}", describe(&root, document));
                    lines.lock().expect("no thread panicked")[index] = line;
                }
            });
        }
    });
    let actual = lines.into_inner().expect("no thread panicked");
    let count = |part: &str| actual.iter().filter(|line| line.contains(part)).count();
    let ok = count(" ok: ");
    let header = format!(
        "# The inputs of each CWL document of graph_test_workflows.txt (decision 24): `ok:` and\n\
         # the counts of its schema, of a check of a simulated run without a job file, and of\n\
         # the inputs that its process reads and its schema does not declare or the other way\n\
         # round; or why it has no schema. {} documents: {ok} schemas, {} invalid, {} fail to\n\
         # load; {} with inputs that the process never reads.",
        actual.len(),
        count(" invalid schema: "),
        actual.len() - ok - count(" invalid schema: "),
        actual.len() - count(" 0 declared not read"),
    );
    println!("{header}");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let text = format!("{header}\n{}\n", actual.join("\n"));
    if std::env::var_os("UPDATE_FIXTURE").is_some() {
        std::fs::write(&fixture, &text).expect("writes the fixture");
    }
    let recorded = std::fs::read_to_string(&fixture).unwrap_or_default();
    let recorded: Vec<&str> = recorded
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect();
    let document = |line: &str| line.split(' ').next().unwrap_or_default().to_owned();
    let offline: Vec<String> = (actual.iter().map(String::as_str))
        .chain(recorded.iter().copied())
        .filter(|line| line.contains(NO_NETWORK))
        .map(document)
        .collect();
    let changed: Vec<&str> = actual
        .iter()
        .map(String::as_str)
        .filter(|line| !recorded.contains(line) && !offline.contains(&document(line)))
        .collect();
    let written = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("inputs_test_workflows.txt");
    std::fs::write(&written, &text).expect("writes the new fixture");
    assert!(
        changed.is_empty(),
        "{} documents differ from {FIXTURE}; the new fixture is in {}:\n{}",
        changed.len(),
        written.display(),
        changed.join("\n")
    );
    let invalid: Vec<&String> = actual
        .iter()
        .filter(|line| line.contains(" invalid schema: "))
        .collect();
    assert!(
        invalid.is_empty(),
        "schemas that are not valid: {invalid:?}"
    );
}
