//! Lowers the CWL v1.2 conformance tests into wir, runs each natively and lowered on the
//! simulator, and compares the counts with the recorded ones.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use cwl_runtime::{CwlDriver, Options, Value};
use serde_json::{Value as Json, json};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Driver, Launch, Simulator, execute};

/// How many tests lower fully, partly or not at all, how many lowered runs end as the native
/// run does (both succeed with as many tasks, or both fail), and how many of those that both
/// succeed give equal outputs, with Files and Directories compared by class and basename.
const RECORDED: Counts = Counts {
    full: 163,
    partly: 194,
    unlowered: 21,
    same: 329,
    equal: 305,
};

#[derive(Debug, Default, PartialEq)]
struct Counts {
    full: usize,
    partly: usize,
    unlowered: usize,
    same: usize,
    equal: usize,
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
fn run(driver: &mut dyn Driver, name: &str, root: &Path) -> Result<usize, String> {
    let mut simulator = Simulator::new(root.join(format!("simulator-{name}")));
    let execution = execute(name, driver, &mut simulator);
    let graph = execution.into_result().map_err(|error| error.to_string())?;
    Ok(graph.nodes.len())
}

/// Outputs with each File and Directory as its class and basename, as the folders differ.
fn shown(value: Json) -> Json {
    match value {
        Json::Array(items) => Json::Array(items.into_iter().map(shown).collect()),
        Json::Object(object) => match object.get("class").and_then(Json::as_str) {
            Some(class @ ("File" | "Directory")) => {
                json!({"class": class, "basename": object.get("basename")})
            }
            _ => Json::Object(object.into_iter().map(|(k, v)| (k, shown(v))).collect()),
        },
        other => other,
    }
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
    let mut native = CwlDriver::new(&launch(test.tool.clone()), Options::default());
    let settings = Settings {
        print: false,
        ..Settings::default()
    };
    let mut lowered = WirDriver::new(&launch(target), settings);
    let native_run = match &mut native {
        Ok(driver) => run(driver, "native", root),
        Err(error) => Err(error.message.clone()),
    };
    let lowered_run = match &mut lowered {
        Ok(driver) => run(driver, "lowered", root),
        Err(error) => Err(error.message.clone()),
    };
    let values = match (&native, &lowered, &native_run, &lowered_run) {
        (Ok(one), Ok(other), Ok(_), Ok(_)) => {
            let given = one.outputs().map(|map| Value::Map(map.clone()).to_json());
            let given = given.and_then(|text| serde_json::from_str(&text).ok());
            Some((given.map(shown), other.outputs().map(shown)))
        }
        _ => None,
    };
    let (native, lowered) = (native_run, lowered_run);
    let ended = match (&native, &lowered) {
        (Ok(one), Ok(other)) if one == other => match values {
            Some((Some(one), Some(other))) if one == other => {
                counts.equal += 1;
                "same, equal outputs".to_owned()
            }
            _ => "same, other outputs".to_owned(),
        },
        (Err(_), Err(_)) => "same".to_owned(),
        (Ok(one), Ok(other)) => format!("differs: {one} and {other} tasks"),
        (Ok(_), Err(error)) => format!("differs: lowered fails: {}", first(error)),
        (Err(error), Ok(_)) => format!("differs: native fails: {}", first(error)),
    };
    if ended.starts_with("same") {
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
