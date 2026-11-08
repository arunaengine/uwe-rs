//! Lowers the examples of the WDL 1.2.1 specification into wir, runs each natively and lowered
//! on the simulator, and compares the counts with the recorded ones.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use serde_json::{Value as Json, json};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Driver, Launch, Simulator, execute};

/// How many examples lower fully, partly or not at all, how many lowered runs end as the
/// native run does (both succeed with as many tasks, or both fail), and how many of those that
/// both succeed give equal outputs, with Files and Directories compared by class and basename.
const RECORDED: Counts = Counts {
    full: 77,
    partly: 73,
    unlowered: 20,
    same: 150,
    equal: 137,
};

#[derive(Debug, Default, PartialEq)]
struct Counts {
    full: usize,
    partly: usize,
    unlowered: usize,
    same: usize,
    equal: usize,
}

/// An example: its document, its input file, and the workflow or task it runs.
struct Example {
    document: PathBuf,
    inputs: Option<PathBuf>,
    target: Option<String>,
}

/// The examples in a folder, by name, with the target of their test config or input keys.
fn examples(root: &Path) -> Vec<Example> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(root)
        .expect("lists the examples")
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|extension| extension == "wdl"))
        .collect();
    names.sort();
    let read = |path: &Path| -> Option<Json> {
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
    };
    names
        .into_iter()
        .map(|document| {
            let stem = document.with_extension("");
            let inputs = stem.with_extension("inputs.json");
            let config = read(&stem.with_extension("config.json")).unwrap_or_default();
            let given = read(&inputs).unwrap_or_default();
            let first = given
                .as_object()
                .and_then(|object| object.keys().next().cloned());
            let prefix = first.and_then(|key| Some(key.split_once('.')?.0.to_owned()));
            let target = config["target"].as_str().map(str::to_owned).or(prefix);
            Example {
                inputs: inputs.exists().then_some(inputs),
                document,
                target,
            }
        })
        .collect()
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

/// The outcome of one example: how it lowers, and whether its lowered run ends as its native
/// one.
fn outcome(example: &Example, counts: &mut Counts) -> String {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let root = folder.path();
    let entry = example.target.as_deref();
    let lowering = match wdl_lowering::lower(&example.document, entry, root) {
        Ok(lowering) => lowering,
        Err(error) => {
            counts.unlowered += 1;
            return format!("unlowered: {}", first(&error));
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
    let base = example.document.parent().unwrap_or(Path::new(""));
    let given = (example.inputs.as_ref())
        .and_then(|path| serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok());
    let converted = lowering.inputs(&given.unwrap_or(json!({})), base);
    let values = root.join("inputs.json");
    if let Ok(converted) = &converted {
        std::fs::write(&values, converted.to_string()).expect("writes the inputs");
    }
    let launch = |target: PathBuf, inputs: Option<PathBuf>| Launch {
        target,
        inputs,
        targets: entry.map(str::to_owned).into_iter().collect(),
        scratch: root.join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let options = wdl_runtime::Options {
        print: false,
        ..Default::default()
    };
    let native_launch = launch(example.document.clone(), example.inputs.clone());
    let mut native = wdl_runtime::WdlDriver::new(&native_launch, options);
    let settings = Settings {
        print: false,
        ..Settings::default()
    };
    let mut lowered = match &converted {
        Ok(_) => WirDriver::new(&launch(target, Some(values)), settings),
        Err(error) => Err(workflow_graph::DriverError::new(error.clone())),
    };
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
            Some((one.exported().map(shown), other.outputs().map(shown)))
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
            Some((one, other)) => format!("same, other outputs: {one:?} and {other:?}"),
            None => "same, other outputs".to_owned(),
        },
        (Err(one), Err(_)) => format!("same, both fail: {}", first(one)),
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
        .take(200)
        .collect()
}

#[test]
#[ignore = "needs the examples of the WDL specification that `just wdl-spec-tests` fetches"]
fn matches_recorded_coverage() {
    let root = std::env::var("WDL_SPEC_TESTS").expect("WDL_SPEC_TESTS names the folder");
    let mut counts = Counts::default();
    for example in examples(Path::new(&root)) {
        let line = outcome(&example, &mut counts);
        if std::env::var("WDL_TEST").is_ok() {
            let name = example.document.file_name().unwrap_or_default();
            println!("{} {line}", name.to_string_lossy());
        }
    }
    println!("{counts:?}");
    assert_eq!(counts, RECORDED);
}
