//! Lowers the CWL v1.2 conformance tests into wir, runs each natively and lowered on the
//! simulator, and compares the counts with the recorded ones.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use cwl_runtime::{CwlDriver, Options, Value};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Driver, Launch, Simulator, execute};

/// How many tests lower fully, partly or not at all, and how many lowered runs end as the
/// native run does: both succeed with as many tasks, or both fail.
const RECORDED: Counts = Counts {
    full: 24,
    partly: 311,
    unlowered: 43,
    same: 315,
};

#[derive(Debug, Default, PartialEq)]
struct Counts {
    full: usize,
    partly: usize,
    unlowered: usize,
    same: usize,
}

struct Test {
    id: String,
    tool: PathBuf,
    job: Option<PathBuf>,
}

/// The tests of a list, following `$import`, with paths from the folder of the list.
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
        let id = (entry.text("id").or(entry.text("label"))).unwrap_or("unnamed");
        tests.push(Test {
            id: id.to_owned(),
            tool: folder.join(tool),
            job: entry.text("job").map(|job| folder.join(job)),
        });
    }
    tests
}

/// The number of tasks of a simulated run, or its error.
fn run(driver: Result<Box<dyn Driver>, String>, name: &str, root: &Path) -> Result<usize, String> {
    let mut driver = driver?;
    let mut simulator = Simulator::new(root.join(format!("simulator-{name}")));
    let execution = execute(name, driver.as_mut(), &mut simulator);
    let graph = execution.into_result().map_err(|error| error.to_string())?;
    Ok(graph.nodes.len())
}

/// The outcome of one test: how it lowers, and whether its lowered run ends as its native one.
fn outcome(test: &Test, counts: &mut Counts) -> String {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let root = folder.path();
    let document = match cwl_parser::load_file(&test.tool, Default::default()) {
        Ok(document) => document,
        Err(_) => {
            counts.unlowered += 1;
            return "unlowered: does not load".into();
        }
    };
    let lowering = match cwl_lowering::lower(&document, root) {
        Ok(lowering) => lowering,
        Err(error) => {
            counts.unlowered += 1;
            return format!("unlowered: {}", error.lines().next().unwrap_or_default());
        }
    };
    let shape = match lowering.kept.first() {
        None => {
            counts.full += 1;
            "full".to_owned()
        }
        Some(kept) => {
            counts.partly += 1;
            format!("partly ({}: {})", kept.path, kept.reason)
        }
    };
    let target = root.join("lowered.wir.json");
    let text = serde_json::to_string(&lowering.document).expect("serializes");
    std::fs::write(&target, text).expect("writes the lowered document");
    let launch = |target: PathBuf| Launch {
        target,
        inputs: test.job.clone(),
        scratch: root.join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let native = CwlDriver::new(&launch(test.tool.clone()), Options::default())
        .map(|driver| Box::new(driver) as Box<dyn Driver>)
        .map_err(|error| error.message);
    let settings = Settings {
        print: false,
        ..Settings::default()
    };
    let lowered = WirDriver::new(&launch(target), settings)
        .map(|driver| Box::new(driver) as Box<dyn Driver>)
        .map_err(|error| error.message);
    let (native, lowered) = (run(native, "native", root), run(lowered, "lowered", root));
    let ended = match (&native, &lowered) {
        (Ok(one), Ok(other)) if one == other => "same".to_owned(),
        (Err(_), Err(_)) => "same".to_owned(),
        (Ok(one), Ok(other)) => format!("differs: {one} and {other} tasks"),
        (Ok(_), Err(error)) => format!("differs: lowered fails: {}", first(error)),
        (Err(error), Ok(_)) => format!("differs: native fails: {}", first(error)),
    };
    if ended == "same" {
        counts.same += 1;
    }
    format!("{shape}, {ended}")
}

fn first(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(160)
        .collect()
}

#[test]
#[ignore = "needs the CWL test workflows that `just cwl-test-workflows` fetches"]
fn matches_recorded_coverage() {
    let root = std::env::var("CWL_TEST_WORKFLOWS").expect("CWL_TEST_WORKFLOWS names the folder");
    let list = Path::new(&root).join("conformance/v1.2/conformance_tests.yaml");
    let mut counts = Counts::default();
    for test in tests(&list) {
        let line = outcome(&test, &mut counts);
        if std::env::var("CWL_TEST").is_ok() {
            println!("{} {line}", test.id);
        }
    }
    println!("{counts:?}");
    assert_eq!(counts, RECORDED);
}
