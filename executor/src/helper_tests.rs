//! Checks stage-in and stage-out of the helper with a staging root in a folder.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::*;

fn request(method: &str, path: &Path) -> Request {
    Request {
        method: method.into(),
        url: reqwest::Url::from_file_path(path).unwrap().to_string(),
        headers: Vec::new(),
    }
}

fn input(store: &Path, path: &str, content: &[u8], executable: bool) -> Input {
    let digest = task_model::Digest::sha256(content);
    let source = store.join(digest.hex());
    std::fs::write(&source, content).unwrap();
    Input {
        path: path.into(),
        sha256: digest.hex().into(),
        size: content.len() as u64,
        executable,
        request: request("GET", &source),
    }
}

#[test]
fn places_checked_inputs() {
    let store = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let plan = Plan {
        inputs: vec![
            input(store.path(), "work/in.txt", b"input", false),
            input(store.path(), "mounts/opt/tool/run.sh", b"#!/bin/sh\n", true),
        ],
        directories: vec!["work/empty dir".into(), "home".into()],
        ..Plan::default()
    };
    stage_in(folder.path(), &plan).unwrap();
    let root = folder.path();
    assert_eq!(std::fs::read(root.join("work/in.txt")).unwrap(), b"input");
    let mode = |path: &str| {
        let metadata = std::fs::metadata(root.join(path)).unwrap();
        metadata.permissions().mode() & 0o777
    };
    assert_eq!(mode("work/in.txt"), 0o644);
    assert_eq!(mode("mounts/opt/tool/run.sh"), 0o755);
    assert!(root.join("work/empty dir").is_dir());
    assert!(stage_in(root, &plan).is_err(), "an input placed twice");
    let open = Plan { open: true, ..plan };
    let folder = tempfile::tempdir().unwrap();
    stage_in(folder.path(), &open).unwrap();
    let metadata = std::fs::metadata(folder.path().join("work")).unwrap();
    assert_eq!(metadata.permissions().mode() & 0o777, 0o777);
    let metadata = std::fs::metadata(folder.path().join("work/in.txt")).unwrap();
    assert_eq!(metadata.permissions().mode() & 0o777, 0o666);
}

#[test]
fn refuses_unsafe_inputs() {
    let store = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    for path in ["../out.txt", "/etc/out.txt", "work/./a", "work//a", ""] {
        let plan = Plan {
            inputs: vec![input(store.path(), path, b"x", false)],
            ..Plan::default()
        };
        let error = stage_in(folder.path(), &plan).unwrap_err();
        assert!(error.contains("leaves the folder"), "{path}: {error}");
    }
    std::os::unix::fs::symlink(store.path(), folder.path().join("link")).unwrap();
    let plan = Plan {
        inputs: vec![input(store.path(), "link/a.txt", b"x", false)],
        ..Plan::default()
    };
    assert!(stage_in(folder.path(), &plan).is_err(), "a link on the way");
    assert!(!store.path().join("a.txt").exists());
    let mut wrong = input(store.path(), "work/b.txt", b"right", false);
    wrong.sha256 = task_model::Digest::sha256(b"other").hex().into();
    let plan = Plan {
        inputs: vec![wrong],
        ..Plan::default()
    };
    let error = stage_in(folder.path(), &plan).unwrap_err();
    assert!(error.contains("SHA-256"), "{error}");
    assert!(!folder.path().join("work/b.txt").exists());
}

#[test]
fn sends_captured_outputs() {
    let store = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    std::fs::create_dir_all(root.join(TRANSFER)).unwrap();
    std::fs::create_dir_all(root.join(crate::CONTROL)).unwrap();
    std::fs::write(root.join("transfer/1"), "first").unwrap();
    std::fs::write(root.join("transfer/2"), "second").unwrap();
    std::fs::write(root.join("control/exit"), "0\n").unwrap();
    let target = |name: &str| store.path().join("attempt").join(name);
    let mut plan = Plan {
        files: vec![request("PUT", &target("files/1"))],
        records: ["exit", "usage"]
            .map(|name| Record {
                name: name.into(),
                request: request("PUT", &target(&format!("control/{name}"))),
            })
            .to_vec(),
        ..Plan::default()
    };
    let error = stage_out(root, None, &plan).unwrap_err();
    assert!(error.contains("more than 1 output files"), "{error}");
    plan.files.push(request("PUT", &target("files/2")));
    std::fs::write(root.join(PLAN), serde_json::to_string(&plan).unwrap()).unwrap();
    main(&["out".into(), root.to_string_lossy().into_owned()]).unwrap();
    assert_eq!(std::fs::read(target("files/2")).unwrap(), b"second");
    assert_eq!(std::fs::read(target("control/exit")).unwrap(), b"0\n");
    assert!(
        !target("control/usage").exists(),
        "a record the task did not write"
    );
}

#[test]
fn installs_this_program() {
    let folder = tempfile::tempdir().unwrap();
    let file = folder.path().join("iunctus");
    main(&["install".into(), file.to_string_lossy().into_owned()]).unwrap();
    let metadata = std::fs::metadata(&file).unwrap();
    assert_eq!(metadata.permissions().mode() & 0o777, 0o755);
    assert_eq!(
        metadata.len(),
        std::fs::metadata(std::env::current_exe().unwrap())
            .unwrap()
            .len()
    );
}

#[test]
fn keeps_plan_apart() {
    let store = tempfile::tempdir().unwrap();
    let node = tempfile::tempdir().unwrap();
    let (data, control) = (node.path().join("data"), node.path().join("control"));
    std::fs::create_dir_all(&control).unwrap();
    let target = |name: &str| store.path().join(name);
    let record = |name: &str| Record {
        name: name.into(),
        request: request("PUT", &target(name)),
    };
    let plan = Plan {
        directories: vec!["control".into()],
        records: vec![record("exit"), record("slurm.log")],
        ..Plan::default()
    };
    let file = store.path().join("plan.json");
    std::fs::write(&file, serde_json::to_string(&plan).unwrap()).unwrap();
    let text = |path: &Path| path.to_string_lossy().into_owned();
    main(&["in".into(), text(&data), text(&file), text(&control)]).unwrap();
    assert!(control.join(PLAN).is_file());
    assert!(!data.join(PLAN).exists(), "the task could change the plan");
    // The log of the job wins over one that the task wrote.
    std::fs::write(data.join("control/slurm.log"), "task").unwrap();
    std::fs::write(control.join("slurm.log"), "job").unwrap();
    std::fs::write(data.join("control/exit"), "0\n").unwrap();
    main(&["out".into(), text(&data), text(&control)]).unwrap();
    assert_eq!(std::fs::read(target("slurm.log")).unwrap(), b"job");
    assert_eq!(std::fs::read(target("exit")).unwrap(), b"0\n");
    for name in ["../../plan.json", "/etc/passwd", "a\0b", ""] {
        let mut outside = record("exit");
        outside.name = name.into();
        let plan = Plan {
            records: vec![outside],
            ..Plan::default()
        };
        let error = stage_out(&data, Some(&control), &plan).unwrap_err();
        assert!(error.contains("leaves the folder"), "{name:?}: {error}");
    }
}

#[test]
fn stalled_download_fails() {
    use std::io::{BufRead, BufReader};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    // The server sends a part of the body and then nothing more, but keeps the connection.
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        while reader.read_line(&mut line).is_ok_and(|count| count > 2) {
            line.clear();
        }
        let mut stream = stream;
        let head = "HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n0123456789";
        stream.write_all(head.as_bytes()).unwrap();
        stream.flush().unwrap();
        std::thread::sleep(Duration::from_secs(60));
    });
    let folder = tempfile::tempdir().unwrap();
    let input = Input {
        path: "work/in.txt".into(),
        sha256: "00".repeat(32),
        size: 100,
        executable: false,
        request: Request {
            method: "GET".into(),
            url: format!("http://{address}/in.txt"),
            headers: Vec::new(),
        },
    };
    let path = folder.path().join("in.txt");
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let client = client().unwrap();
        let _ = sender.send(download(&client, &input, &path, false));
    });
    let result = receiver.recv_timeout(Duration::from_secs(30));
    let error = result.expect("a stalled download ends").unwrap_err();
    assert!(!error.is_empty());
}
