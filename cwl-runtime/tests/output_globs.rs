//! Finite output globs inspect only relevant paths; recursive listings report cycles.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use cwl_runtime::{CwlDriver, Options, Value};
use workflow_graph::{Driver, Launch, Outcome, OutputFile, Task};

fn start(root: &Path, pattern: &str, options: Options) -> (CwlDriver, Task) {
    std::fs::write(
        root.join("main.cwl"),
        format!(
            "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: 'true'\ninputs: []\n\
         outputs: {{out: {{type: 'File[]', outputBinding: {{glob: '{pattern}'}}}}}}\n"
        ),
    )
    .expect("tool");
    std::fs::write(root.join("job.json"), "{}").expect("job");
    let launch = Launch {
        target: root.join("main.cwl"),
        inputs: Some(root.join("job.json")),
        scratch: root.join("scratch"),
        ..Launch::default()
    };
    let mut driver = CwlDriver::new(&launch, options).expect("driver");
    let mut tasks = driver.start().expect("starts");
    (driver, tasks.remove(0))
}

fn directory(location: &str) -> OutputFile {
    OutputFile {
        path: "dir".into(),
        location: location.into(),
        directory: true,
        ..OutputFile::default()
    }
}

#[test]
fn finite_ignores_cycles() {
    for (pattern, reported) in [
        ("dir/good.txt", true),
        ("dir/good.txt", false),
        ("dir/*.txt", false),
    ] {
        let root = tempfile::tempdir().expect("folder");
        let dir = root.path().join("work/dir");
        std::fs::create_dir_all(dir.join("a")).expect("directory");
        std::fs::write(dir.join("good.txt"), "good").expect("file");
        std::os::unix::fs::symlink("..", dir.join("a/parent")).expect("cycle");
        std::os::unix::fs::symlink("missing", dir.join("unrelated")).expect("dangling link");
        let (mut driver, task) = start(
            root.path(),
            pattern,
            Options {
                simulated: false,
                ..Options::default()
            },
        );
        let mut files = vec![directory(&dir.to_string_lossy())];
        if reported {
            files.push(OutputFile {
                path: "dir/good.txt".into(),
                location: dir.join("good.txt").to_string_lossy().into_owned(),
                ..OutputFile::default()
            });
        }
        driver
            .complete(Outcome {
                id: task.id,
                files,
                ..Outcome::default()
            })
            .expect("outcome");
        let output = driver
            .outputs()
            .expect("successful collection")
            .get("out")
            .and_then(Value::as_list)
            .expect("files");
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].text("basename"), Some("good.txt"));
        assert!(!driver.draining());
    }
}

#[test]
fn wildcards_skip_hidden() {
    let root = tempfile::tempdir().expect("folder");
    let dir = root.path().join("work/dir");
    std::fs::create_dir_all(dir.join(".hidden")).expect("directory");
    for name in ["visible.txt", ".hidden.txt", ".hidden/nested.txt"] {
        std::fs::write(dir.join(name), "output").expect("file");
    }
    for (pattern, expected) in [
        ("dir/*.txt", vec!["visible.txt"]),
        ("dir/?.txt", vec![]),
        ("dir/**/*.txt", vec![]),
        ("dir/.*.txt", vec![".hidden.txt"]),
        ("dir/.hidden/*.txt", vec!["nested.txt"]),
        (
            "dir/{visible,.hidden}.txt",
            vec![".hidden.txt", "visible.txt"],
        ),
    ] {
        let (mut driver, task) = start(
            root.path(),
            pattern,
            Options {
                simulated: false,
                ..Options::default()
            },
        );
        let mut files = vec![directory(&dir.to_string_lossy())];
        for name in ["visible.txt", ".hidden.txt", ".hidden/nested.txt"] {
            files.push(OutputFile {
                path: format!("dir/{name}"),
                location: dir.join(name).to_string_lossy().into_owned(),
                ..OutputFile::default()
            });
        }
        driver
            .complete(Outcome {
                id: task.id,
                files,
                ..Outcome::default()
            })
            .expect("outcome");
        let names: Vec<&str> = driver.outputs().expect("outputs")["out"]
            .as_list()
            .expect("files")
            .iter()
            .map(|file| file.text("basename").expect("basename"))
            .collect();
        assert_eq!(names, expected, "{pattern}");
    }
}

#[test]
fn preserves_glob_aliases() {
    let root = tempfile::tempdir().expect("folder");
    let dir = root.path().join("work/dir");
    std::fs::create_dir_all(dir.join("real")).expect("directory");
    std::fs::write(dir.join("real/good.txt"), "good").expect("file");
    std::os::unix::fs::symlink("real", dir.join("alias")).expect("alias");
    let (mut driver, task) = start(
        root.path(),
        "dir/{real,alias}/*.txt",
        Options {
            simulated: false,
            ..Options::default()
        },
    );
    driver
        .complete(Outcome {
            id: task.id,
            files: vec![directory(&dir.to_string_lossy())],
            ..Outcome::default()
        })
        .expect("outcome");
    let output = driver
        .outputs()
        .expect("successful collection")
        .get("out")
        .and_then(Value::as_list)
        .expect("files");
    assert_eq!(output.len(), 2);
    assert_ne!(output[0].text("location"), output[1].text("location"));
    assert_eq!(output[0].get("checksum"), output[1].get("checksum"));
}

#[test]
fn reads_ancestor_aliases() {
    let root = tempfile::tempdir().expect("folder");
    let dir = root.path().join("work/dir");
    std::fs::create_dir_all(dir.join("a")).expect("directory");
    std::fs::write(dir.join("good.txt"), "good").expect("file");
    std::os::unix::fs::symlink("..", dir.join("a/parent")).expect("ancestor alias");
    for pattern in [
        "dir/a/parent/good.txt",
        "dir/*/parent/good.txt",
        "{dir/a/parent/good.txt,unrelated/**}",
    ] {
        let (mut driver, task) = start(
            root.path(),
            pattern,
            Options {
                simulated: false,
                ..Options::default()
            },
        );
        driver
            .complete(Outcome {
                id: task.id,
                files: vec![directory(&dir.to_string_lossy())],
                ..Outcome::default()
            })
            .expect("outcome");
        let output = driver
            .outputs()
            .expect("bounded alias collection")
            .get("out")
            .and_then(Value::as_list)
            .expect("files");
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].text("basename"), Some("good.txt"));
        assert_eq!(output[0].get("size"), Some(&Value::Integer(4)));
    }
}

#[test]
fn reports_recursive_cycles() {
    let root = tempfile::tempdir().expect("folder");
    let dir = root.path().join("work/dir");
    std::fs::create_dir_all(dir.join("a")).expect("directory");
    std::os::unix::fs::symlink("..", dir.join("a/parent")).expect("cycle");
    let (mut driver, task) = start(
        root.path(),
        "dir/**/*.txt",
        Options {
            simulated: false,
            ..Options::default()
        },
    );
    driver
        .complete(Outcome {
            id: task.id,
            files: vec![directory(&dir.to_string_lossy())],
            ..Outcome::default()
        })
        .expect("outcome");
    let error = driver.finish().expect_err("cycle").message;
    assert!(error.contains("directory cycle"), "{error}");
    assert!(
        !error.contains("Too many levels"),
        "cycle is detected before filesystem exhaustion: {error}"
    );
}

#[test]
fn expands_remote_globs() {
    let root = tempfile::tempdir().expect("folder");
    std::fs::create_dir_all(root.path().join("work/dir/sub")).expect("directory");
    std::fs::write(root.path().join("work/dir/sub/a b.txt"), "remote").expect("file");
    let config = config_file::ConfigFile::parse(
        &format!(
            "[storage.mirror]\ntype = \"folder\"\nprefixes = [\"s3://mirror/\"]\npath = \"{}\"\n",
            root.path().display()
        ),
        Path::new("config.toml"),
    )
    .expect("config");
    let storage = storage::Storage::from_config(&config).expect("provider");
    let options = Options {
        simulated: false,
        remote: cwl_parser::source::Remote::new(&storage, root.path().join("downloads")),
        ..Options::default()
    };
    let (mut driver, task) = start(root.path(), "dir/sub/*.txt", options);
    driver
        .complete(Outcome {
            id: task.id,
            files: vec![directory("s3://mirror/work/dir")],
            ..Outcome::default()
        })
        .expect("outcome");
    if driver.outputs().is_none() {
        panic!("collection failed: {:?}", driver.finish());
    }
    let output = driver
        .outputs()
        .expect("successful collection")
        .get("out")
        .and_then(Value::as_list)
        .expect("files");
    assert_eq!(output.len(), 1);
    assert_eq!(
        output[0].text("location"),
        Some("s3://mirror/work/dir/sub/a%20b.txt")
    );
    assert_eq!(output[0].get("size"), Some(&Value::Integer(6)));
}
