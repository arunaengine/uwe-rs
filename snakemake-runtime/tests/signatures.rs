//! Successful evaluated signatures publish after acknowledged delivery, never onto old outputs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use workflow_graph::{
    Attempt, Driver, Engine, Executor, Files, Launch, LocalFiles, Outcome, OutputFile, Running,
};

fn record(folder: &Path, output: &str) -> PathBuf {
    folder
        .join(".snakemake/iunctus-signatures")
        .join(base64::engine::general_purpose::URL_SAFE.encode(output))
}

fn driver(folder: &Path, text: &str) -> Box<dyn Driver> {
    std::fs::write(folder.join("Snakefile"), text).unwrap();
    snakemake_runtime::driver(&Launch {
        target: folder.to_path_buf(),
        scratch: folder.join("scratch"),
        ..Launch::default()
    })
    .unwrap()
}

fn outcome(id: workflow_graph::TaskId, source: &Path, outputs: &[&str]) -> Outcome {
    Outcome {
        id,
        files: outputs
            .iter()
            .map(|output| OutputFile {
                path: (*output).into(),
                location: source.to_string_lossy().into_owned(),
                ..OutputFile::default()
            })
            .collect(),
        ..Outcome::default()
    }
}

const SINGLE: &str = r#"
rule make:
    input: source="in.txt"
    output: "first.txt", "second.txt"
    params: unused=lambda wildcards: 2
    container: "debian:stable-slim"
    shell: "echo fixed"
"#;

#[test]
fn acknowledges_signature_delivery() {
    use std::os::unix::fs::PermissionsExt;
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(folder.path().join("in.txt"), "input").unwrap();
    let source = folder.path().join("raw");
    std::fs::write(&source, "raw CAS bytes").unwrap();
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o444)).unwrap();
    let mut driver = driver(folder.path(), SINGLE);
    let task = driver.start().unwrap().remove(0);
    driver
        .complete(outcome(task.id, &source, &["first.txt", "second.txt"]))
        .unwrap();
    let deliveries = driver.deliveries();
    assert!(!record(folder.path(), "first.txt").exists());
    LocalFiles.deliver(&deliveries[0]).unwrap();
    driver.delivered(&deliveries[0]).unwrap();
    assert!(!record(folder.path(), "first.txt").exists());
    LocalFiles.deliver(&deliveries[1]).unwrap();
    driver.delivered(&deliveries[1]).unwrap();
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(record(folder.path(), "first.txt")).unwrap())
            .unwrap();
    assert_eq!(value["version"], 1);
    assert_eq!(value["params"], serde_json::json!([["int", "2"]]));
    assert_eq!(
        value["param_names"],
        serde_json::json!([["unused", 0, null]])
    );
    assert_eq!(value["inputs"], serde_json::json!(["in.txt"]));
    assert_eq!(value["image"], "debian:stable-slim");
    assert_eq!(value["command"], serde_json::json!(task.command));
    assert_eq!(
        std::fs::read(record(folder.path(), "first.txt")).unwrap(),
        std::fs::read(record(folder.path(), "second.txt")).unwrap()
    );
    assert_eq!(std::fs::read(&source).unwrap(), b"raw CAS bytes");
    assert_eq!(
        std::fs::metadata(&source).unwrap().permissions().mode() & 0o777,
        0o444
    );
    driver.finish().unwrap();
}

struct DeliveredOutputs {
    source: PathBuf,
    fail_second: bool,
}
impl Executor for DeliveredOutputs {
    fn run(&mut self, attempt: Attempt) -> Running {
        let outputs: Vec<_> = attempt.task.outputs.iter().map(String::as_str).collect();
        Running::ended(Ok(outcome(attempt.task.id, &self.source, &outputs)))
    }
    fn files(&self) -> Option<Arc<dyn Files>> {
        Some(Arc::new(DeliveredFiles {
            fail_second: self.fail_second,
        }))
    }
}

struct DeliveredFiles {
    fail_second: bool,
}
impl Files for DeliveredFiles {
    fn local(&self, location: &str) -> std::io::Result<PathBuf> {
        LocalFiles.local(location)
    }
    fn deliver(&self, delivery: &workflow_graph::Delivery) -> std::io::Result<()> {
        if self.fail_second
            && Path::new(&delivery.target)
                .file_name()
                .is_some_and(|name| name == "second.txt")
        {
            return Err(std::io::Error::other("injected second delivery failure"));
        }
        LocalFiles.deliver(delivery)
    }
}

#[test]
fn preserves_prior_records() {
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(folder.path().join("in.txt"), "input").unwrap();
    let source = folder.path().join("raw");
    std::fs::write(&source, "made").unwrap();
    let mut original = driver(folder.path(), SINGLE);
    let mut executor = DeliveredOutputs {
        source: source.clone(),
        fail_second: false,
    };
    assert!(
        Engine::new(&mut executor)
            .run("first", original.as_mut())
            .error
            .is_none()
    );
    let prior = std::fs::read(record(folder.path(), "first.txt")).unwrap();
    let changed_source = folder.path().join("changed");
    std::fs::write(&changed_source, "changed bytes").unwrap();
    executor.source = changed_source;
    let mut next = driver(folder.path(), &SINGLE.replace("echo fixed", "echo changed"));
    executor.fail_second = true;
    assert!(
        Engine::new(&mut executor)
            .run("second", next.as_mut())
            .error
            .is_some()
    );
    assert_eq!(
        std::fs::read(record(folder.path(), "first.txt")).unwrap(),
        prior
    );
    assert_eq!(
        std::fs::read(folder.path().join("first.txt")).unwrap(),
        b"changed bytes"
    );
    assert_eq!(
        std::fs::read(folder.path().join("second.txt")).unwrap(),
        b"made"
    );
    let mut restored = driver(folder.path(), SINGLE);
    assert_eq!(restored.start().unwrap().len(), 1);
    drop(restored);
    std::fs::remove_dir_all(folder.path().join(".snakemake/iunctus-signatures")).unwrap();
    let mut missing_records = driver(folder.path(), SINGLE);
    assert_eq!(missing_records.start().unwrap().len(), 1);
    drop(missing_records);
    executor.source = source;
    executor.fail_second = false;
    let mut repaired = driver(folder.path(), SINGLE);
    assert!(
        Engine::new(&mut executor)
            .run("repaired", repaired.as_mut())
            .error
            .is_none()
    );
    assert!(
        std::fs::read_dir(folder.path().join(".snakemake/iunctus-incomplete"))
            .unwrap()
            .next()
            .is_none()
    );
    std::fs::remove_dir_all(folder.path().join(".snakemake/iunctus-signatures")).unwrap();
    let mut legacy = driver(folder.path(), SINGLE);
    assert!(legacy.start().unwrap().is_empty());
    legacy.finish().unwrap();
    assert!(!record(folder.path(), "first.txt").exists());
}

#[test]
fn acknowledges_group_delivery() {
    let folder = tempfile::tempdir().unwrap();
    let text = r#"
rule all:
    input: "first.txt", "second.txt"
rule first:
    input: "socket"
    output: "first.txt"
    shell: "echo first"
rule server:
    output: service("socket")
    shell: "touch socket; sleep 1000"
rule second:
    input: "socket"
    output: ensure("second.txt", non_empty=True)
    shell: "echo second"
"#;
    let source = folder.path().join("raw");
    std::fs::write(&source, "made").unwrap();
    let mut driver = driver(folder.path(), text);
    let group = driver.start().unwrap().remove(0);
    driver
        .complete(outcome(group.id, &source, &["first.txt", "second.txt"]))
        .unwrap();
    let deliveries = driver.deliveries();
    driver.delivered(&deliveries[0]).unwrap();
    assert!(!record(folder.path(), "first.txt").exists());
    driver.delivered(&deliveries[1]).unwrap();
    assert!(record(folder.path(), "first.txt").exists());
    assert!(record(folder.path(), "second.txt").exists());
    let before = std::fs::read(record(folder.path(), "first.txt")).unwrap();
    drop(driver);
    for failure in ["exit", "missing", "empty"] {
        let mut failed = self::driver(folder.path(), text);
        let group = failed.start().unwrap().remove(0);
        let mut bad = outcome(group.id, &source, &["first.txt"]);
        bad.exit_code = if failure == "exit" { 1 } else { 0 };
        if failure == "empty" {
            let empty = folder.path().join("empty");
            std::fs::write(&empty, "").unwrap();
            bad.files
                .extend(outcome(group.id, &empty, &["second.txt"]).files);
        }
        failed.complete(bad).unwrap();
        assert!(failed.deliveries().is_empty());
        assert_eq!(
            std::fs::read(record(folder.path(), "first.txt")).unwrap(),
            before
        );
    }
}

#[test]
fn compares_evaluated_signatures() {
    let folder = tempfile::tempdir().unwrap();
    for name in ["in.txt", "other.txt"] {
        std::fs::write(folder.path().join(name), "input").unwrap();
    }
    let source = folder.path().join("raw");
    std::fs::write(&source, "made").unwrap();
    let mut executor = DeliveredOutputs {
        source,
        fail_second: false,
    };
    let mut first = driver(folder.path(), SINGLE);
    assert!(
        Engine::new(&mut executor)
            .run("first", first.as_mut())
            .error
            .is_none()
    );
    let mut unchanged = driver(folder.path(), SINGLE);
    assert!(unchanged.start().unwrap().is_empty());
    unchanged.finish().unwrap();
    for changed in [
        SINGLE.replace("wildcards: 2", "wildcards: 3"),
        SINGLE.replace("echo fixed", "echo changed"),
        SINGLE.replace("debian:stable-slim", "debian:bookworm-slim"),
        SINGLE.replace("in.txt", "other.txt"),
        SINGLE.replace("source=", "renamed="),
    ] {
        let mut changed = driver(folder.path(), &changed);
        assert_eq!(changed.start().unwrap().len(), 1);
    }
    std::fs::write(record(folder.path(), "first.txt"), "null").unwrap();
    let mut damaged = driver(folder.path(), SINGLE);
    assert_eq!(damaged.start().unwrap().len(), 1);
}

#[test]
fn propagates_signature_changes() {
    let folder = tempfile::tempdir().unwrap();
    let text = r#"
calls = []
def parameter(wildcards):
    calls.append(1)
    if len(calls) > 1:
        raise ValueError("params evaluated twice")
    return 2
rule all:
    input: "final.txt"
rule first:
    output: "first.txt"
    params: unused=parameter
    shell: "echo first"
rule last:
    input: "first.txt"
    output: "final.txt"
    shell: "echo last"
"#;
    let source = folder.path().join("raw");
    std::fs::write(&source, "made").unwrap();
    let mut executor = DeliveredOutputs {
        source,
        fail_second: false,
    };
    let mut first = driver(folder.path(), text);
    assert!(
        Engine::new(&mut executor)
            .run("first", first.as_mut())
            .error
            .is_none()
    );
    let mut changed = driver(folder.path(), &text.replace("return 2", "return 3"));
    let task = changed.start().unwrap().remove(0);
    assert_eq!(task.step, "first");
    let next = changed
        .complete(outcome(task.id, &executor.source, &["first.txt"]))
        .unwrap();
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].step, "last");
}

#[test]
fn protects_delivered_outputs() {
    use std::os::unix::fs::PermissionsExt;
    let folder = tempfile::tempdir().unwrap();
    let source = folder.path().join("raw");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("data"), "made").unwrap();
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o750)).unwrap();
    std::fs::set_permissions(source.join("data"), std::fs::Permissions::from_mode(0o640)).unwrap();
    let text =
        "rule make:\n    output: protected(directory(\"result\"))\n    shell: \"echo first\"\n";
    let mut first = driver(folder.path(), text);
    let task = first.start().unwrap().remove(0);
    let mut made = outcome(task.id, &source, &["result"]);
    made.files[0].directory = true;
    first.complete(made).unwrap();
    for delivery in first.deliveries() {
        LocalFiles.deliver(&delivery).unwrap();
        first.delivered(&delivery).unwrap();
    }
    for path in [
        folder.path().join("result"),
        folder.path().join("result/data"),
    ] {
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o222,
            0
        );
    }
    assert_eq!(
        std::fs::metadata(&source).unwrap().permissions().mode() & 0o777,
        0o750
    );
    assert_eq!(
        std::fs::metadata(source.join("data"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o640
    );
    first.finish().unwrap();
    let mut changed = driver(folder.path(), &text.replace("echo first", "echo changed"));
    assert!(
        changed
            .start()
            .unwrap_err()
            .message
            .contains("ProtectedOutputException")
    );
    std::fs::set_permissions(
        folder.path().join("result"),
        std::fs::Permissions::from_mode(0o750),
    )
    .unwrap();
}
