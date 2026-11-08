//! Lowers the CWL v1.2 conformance tests into wir, runs each natively and lowered on the
//! simulator, and compares the counts with the recorded ones.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use cwl_runtime::{CwlDriver, Options, Value};
use serde_json::{Value as Json, json};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Driver, Launch, Simulator, execute};

/// Tests compiled into wir fully, partly or not at all; compiled runs that end as native ones,
/// with outputs equal by class and basename; that end as the test expects (outputs or failure);
/// and with outputs whose Files also have equal sizes and contents.
const RECORDED: Counts = Counts {
    full: 294,
    partly: 65,
    unlowered: 19,
    same: 344,
    equal: 316,
    expected: 138,
    exact: 312,
};

#[derive(Debug, Default, PartialEq)]
struct Counts {
    full: usize,
    partly: usize,
    unlowered: usize,
    same: usize,
    equal: usize,
    expected: usize,
    exact: usize,
}

struct Test {
    id: String,
    tool: PathBuf,
    job: Option<PathBuf>,
    output: Json,
    should_fail: bool,
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
        let output = entry.get("output").map(Value::to_json);
        tests.push(Test {
            id: id.to_owned(),
            tool: folder.join(tool),
            job: entry.text("job").map(|job| folder.join(job)),
            output: output
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_default(),
            should_fail: entry.get("should_fail") == Some(&Value::Bool(true)),
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
    let given = match (&lowered, &lowered_run) {
        (Ok(driver), Ok(_)) => driver.outputs().map(wir_runtime::cwl::to_cwl),
        _ => None,
    };
    let expected = match (test.should_fail, &lowered_run, &given) {
        (true, Err(_), _) => true,
        (false, Ok(_), Some(outputs)) => compare(&test.output, outputs).is_ok(),
        _ => false,
    };
    counts.expected += usize::from(expected);
    let values = match (&native, &lowered, &native_run, &lowered_run) {
        (Ok(one), Ok(other), Ok(_), Ok(_)) => {
            let given = one.outputs().map(|map| Value::Map(map.clone()).to_json());
            let given: Option<Json> = given.and_then(|text| serde_json::from_str(&text).ok());
            if let (Some(one), Some(other)) = (&given, other.outputs())
                && contents(one.clone()) == contents(other)
            {
                counts.exact += 1;
            }
            Some((given.map(shown), other.outputs().map(shown)))
        }
        _ => None,
    };
    if std::env::var("CWL_SHOW").is_ok_and(|ids| ids.split(',').any(|id| id == test.id)) {
        eprintln!(
            "SHOW {} native={:?} lowered={:?}",
            test.id, values, lowered_run
        );
    }
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
    let expected = match expected {
        true => ", as expected",
        false => "",
    };
    format!("{shape}, {ended}{expected}")
}

/// Outputs with each File as its class, basename, size and the digest of its content, and
/// each Directory as its class and basename.
fn contents(value: Json) -> Json {
    match value {
        Json::Array(items) => Json::Array(items.into_iter().map(contents).collect()),
        Json::Object(object) => match object.get("class").and_then(Json::as_str) {
            Some("File") => {
                let bytes = local(&object).and_then(|path| std::fs::read(path).ok());
                let digest = bytes.as_deref().map(sha1);
                let size = bytes.as_ref().map(Vec::len);
                json!({"class": "File", "basename": basename(&object), "size": size, "digest": digest})
            }
            Some("Directory") => json!({"class": "Directory", "basename": basename(&object)}),
            _ => Json::Object(object.into_iter().map(|(k, v)| (k, contents(v))).collect()),
        },
        other => other,
    }
}

/// The local path of a File or Directory value, from its location.
fn local(object: &serde_json::Map<String, Json>) -> Option<PathBuf> {
    let location = object.get("location").and_then(Json::as_str)?;
    cwl_parser::iri::file_path(location).or_else(|| Some(PathBuf::from(location)))
}

fn basename(object: &serde_json::Map<String, Json>) -> Option<String> {
    let location = object.get("location").and_then(Json::as_str);
    (object.get("basename").and_then(Json::as_str))
        .or_else(|| location.and_then(|text| text.trim_end_matches('/').rsplit('/').next()))
        .map(str::to_owned)
}

/// `sha1$<hex>` of bytes, as CWL writes checksums.
fn sha1(bytes: &[u8]) -> String {
    use sha1::Digest;
    let digest = sha1::Sha1::digest(bytes);
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("sha1${hex}")
}

/// cwltest's comparison of expected outputs: `Any` matches anything, a File by the end of its
/// location, its checksum, size and contents, a Directory by its listing in any order.
fn compare(expected: &Json, actual: &Json) -> Result<(), String> {
    if expected.as_str() == Some("Any") {
        return Ok(());
    }
    let differ = || Err(format!("expected {expected}, got {actual}"));
    match (expected, actual) {
        (Json::Object(want), Json::Object(got)) if want.contains_key("class") => {
            if want.get("class") != got.get("class") {
                return differ();
            }
            if let Some(name) = want
                .get("location")
                .or(want.get("path"))
                .and_then(Json::as_str)
                && name != "Any"
                && basename(got).as_deref() != name.rsplit('/').next()
            {
                return differ();
            }
            let bytes = local(got).and_then(|path| std::fs::read(path).ok());
            let wanted = |key: &str| want.get(key).filter(|value| value.as_str() != Some("Any"));
            if let Some(checksum) = wanted("checksum")
                && bytes.as_deref().map(sha1).as_deref() != checksum.as_str()
            {
                return differ();
            }
            if let Some(size) = wanted("size")
                && bytes.as_ref().map(|bytes| bytes.len() as u64) != size.as_u64()
            {
                return differ();
            }
            if let Some(text) = wanted("contents")
                && bytes.as_deref().map(String::from_utf8_lossy).as_deref() != text.as_str()
            {
                return differ();
            }
            if let Some(Json::Array(items)) = want.get("listing") {
                let listing = got.get("listing").and_then(Json::as_array);
                let listing = listing.map(Vec::as_slice).unwrap_or_default();
                for item in items {
                    if !listing.iter().any(|other| compare(item, other).is_ok()) {
                        return differ();
                    }
                }
            }
            Ok(())
        }
        (Json::Object(want), Json::Object(got)) => {
            for (key, value) in want {
                compare(value, got.get(key).unwrap_or(&Json::Null))?;
            }
            match (got.iter()).any(|(key, value)| !want.contains_key(key) && !value.is_null()) {
                true => differ(),
                false => Ok(()),
            }
        }
        (Json::Array(want), Json::Array(got)) if want.len() == got.len() => want
            .iter()
            .zip(got)
            .try_for_each(|(one, other)| compare(one, other)),
        (Json::Number(one), Json::Number(other)) if one.as_f64() == other.as_f64() => Ok(()),
        _ if expected == actual => Ok(()),
        _ => differ(),
    }
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
