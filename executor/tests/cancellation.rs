//! Checks cancellation waits for task and line-helper removal through a gated fake engine.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use executor::process::{ContainerProcess, ContainerSpec, ProcessError, ProcessLimits};
use executor::{Backend, CancellationToken, ContainerEngine, Job, LocalContainer, Pool, Staging};
use task_model::{FORMAT, Image, Placement, TaskSpec};

#[test]
fn cancellation_waits_removal() {
    check_removal("cancellation_waits_removal", false, 204);
}

#[test]
fn cancellation_warns_failure() {
    check_removal("cancellation_warns_failure", false, 500);
}

#[test]
fn helper_waits_removal() {
    check_removal("helper_waits_removal", true, 204);
}

#[test]
fn helper_warns_failure() {
    check_removal("helper_warns_failure", true, 500);
}

#[test]
fn cancellation_warns_timeout() {
    check_removal("cancellation_warns_timeout", false, 0);
}

fn check_removal(test: &str, helper: bool, status: u16) {
    if let Ok(socket) = std::env::var("IUNCTUS_CANCEL_ENGINE") {
        probe(Path::new(&socket), helper);
        return;
    }
    let folder = tempfile::tempdir().unwrap();
    let socket = folder.path().join("engine.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let control = UnixListener::bind(folder.path().join("control.sock")).unwrap();
    let (requested, requests) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let server = std::thread::spawn(move || {
        serve(listener, control, requested, released, status);
    });
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env("IUNCTUS_CANCEL_ENGINE", &socket)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let requested = requests.recv_timeout(Duration::from_secs(5));
    let returned = child.try_wait().unwrap();
    let _ = release.send(());
    let output = child.wait_with_output().unwrap();
    let mut stop = UnixStream::connect(&socket).unwrap();
    stop.write_all(b"STOP / HTTP/1.1\r\n\r\n").unwrap();
    server.join().unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(requested.is_ok(), "removal was never requested: {stderr}");
    assert!(returned.is_none(), "run returned before removal replied");
    assert!(output.status.success(), "{stderr}");
    if status == 0 {
        assert!(
            stderr.contains("warning: could not remove container owned-id: ")
                && stderr.contains("removal was not confirmed within 10 seconds"),
            "{stderr}"
        );
    } else if status == 500 {
        assert!(
            stderr.contains("warning: could not remove container owned-id: ")
                && stderr.contains("removal failed"),
            "{stderr}"
        );
    } else {
        assert!(!stderr.contains("warning:"), "{stderr}");
    }
}

fn serve(
    listener: UnixListener,
    control: UnixListener,
    requested: mpsc::Sender<()>,
    released: mpsc::Receiver<()>,
    status: u16,
) {
    let mut control = control.accept().unwrap().0;
    let mut held = Vec::new();
    let mut deleting = false;
    loop {
        let mut stream = listener.accept().unwrap().0;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut first = String::new();
        reader.read_line(&mut first).unwrap();
        if first.starts_with("STOP ") {
            break;
        }
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap();
            }
        }
        reader.read_exact(&mut vec![0; length]).unwrap();
        if first.contains("/attach?") {
            stream.write_all(b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\n").unwrap();
            held.push(stream);
        } else if first.contains("/wait?") {
            held.push(stream);
        } else if first.contains("/start") {
            reply(&mut stream, 204, "");
            control.write_all(b"cancel").unwrap();
        } else if first.starts_with("DELETE ") {
            assert!(first.contains("/containers/owned-id?"), "{first}");
            assert!(first.contains("force=true"), "{first}");
            if !deleting {
                deleting = true;
                reply(&mut stream, 409, r#"{"message":"removal in progress"}"#);
                continue;
            }
            requested.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(5)).unwrap();
            if status == 0 {
                held.push(stream);
            } else {
                reply(&mut stream, status, r#"{"message":"removal failed"}"#);
            }
        } else if first.contains("/kill") {
            reply(&mut stream, 204, "");
        } else if first.contains("/containers/create?") {
            reply(&mut stream, 201, r#"{"Id":"owned-id","Warnings":[]}"#);
        } else if first.contains("/images/") {
            reply(&mut stream, 200, r#"{"Id":"local-image"}"#);
        } else if first.contains("/version") || first.contains("/info") {
            reply(&mut stream, 200, r#"{"MemoryLimit":true,"SwapLimit":true}"#);
        } else {
            panic!("unexpected engine request: {first}");
        }
    }
}

fn reply(stream: &mut UnixStream, status: u16, body: &str) {
    let body = if status == 204 { "" } else { body };
    write!(
        stream,
        "HTTP/1.1 {status} Reply\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
}

fn probe(socket: &Path, helper: bool) {
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    let mut control = UnixStream::connect(socket.with_file_name("control.sock")).unwrap();
    control
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let signal = std::thread::spawn(move || {
        control.read_exact(&mut [0; 6]).unwrap();
        trigger.cancel();
    });
    if helper {
        let spec = ContainerSpec {
            engine: socket.to_str().unwrap().into(),
            name: "helper-name".into(),
            image: "debian:stable-slim".into(),
            memory: Some(64 << 20),
            ..Default::default()
        };
        let result = ContainerProcess::start_bounded(
            &spec,
            ProcessLimits {
                max_frame_bytes: 1024,
                reply_capacity: 1,
            },
            &cancel,
        );
        match result {
            Ok(mut process) => assert!(matches!(
                process.exchange("request"),
                Err(ProcessError::Cancelled)
            )),
            Err(error) => assert!(matches!(error, ProcessError::Cancelled), "{error}"),
        }
    } else {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let engine = runtime
            .block_on(ContainerEngine::connect(socket.to_str().unwrap()))
            .unwrap();
        let backend = LocalContainer::new(
            engine,
            socket.with_file_name("scratch"),
            Pool::new(1, 1 << 30),
        );
        let spec = TaskSpec {
            format: FORMAT,
            namespace: String::new(),
            image: Some(Image {
                reference: "debian:stable-slim".into(),
                platform: None,
            }),
            arguments: vec!["sleep".into(), "120".into()],
            workdir: None,
            environment: Default::default(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            stdin: None,
            containers: Vec::new(),
            replaces_entrypoint: true,
            run_as: None,
        };
        let job = Job {
            name: "task-name",
            spec: &spec,
            staging: &Staging::default(),
            placement: &Placement::default(),
        };
        let result = runtime.block_on(backend.run(job, &cancel));
        assert!(
            matches!(result, Err(executor::ExecutorError::Cancelled)),
            "{result:?}"
        );
    }
    signal.join().unwrap();
}
