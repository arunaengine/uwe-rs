//! Checks container helper exchanges and completion against local Docker or Podman.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use executor::process::{ContainerProcess, ContainerSpec, ProcessError, run_to_end};

fn spec(name: &str, script: &str) -> ContainerSpec {
    ContainerSpec {
        engine: std::env::var("UWE_ENGINE").unwrap_or_default(),
        name: format!("iunctus-test-{name}-{}", std::process::id()),
        image: "debian:stable-slim".into(),
        entrypoint: Some("/bin/bash".into()),
        command: vec!["-c".into(), script.into()],
        memory: Some(64 << 20),
        ..ContainerSpec::default()
    }
}

#[test]
#[ignore = "needs Docker or Podman and the debian:stable-slim image"]
fn exchanges_container_lines() {
    let script = "echo ready >&2; while read line; do echo \"got $line\"; done";
    let mut process = ContainerProcess::start(&spec("answers", script)).expect("starts");
    assert_eq!(process.exchange("a").expect("a reply"), "got a");
    assert_eq!(process.exchange("b c").expect("a reply"), "got b c");
    process.stop();
}

#[test]
#[ignore = "needs Docker or Podman and the debian:stable-slim image"]
fn preserves_conflicting_container() {
    let settings = spec("conflict", "while read line; do echo \"got $line\"; done");
    let mut first = ContainerProcess::start(&settings).expect("starts");
    assert!(ContainerProcess::start(&settings).is_err());
    assert_eq!(first.exchange("a").unwrap(), "got a");
    assert!(run_to_end(&settings).is_err());
    assert_eq!(first.exchange("b").unwrap(), "got b");
    first.stop();
}

#[test]
#[ignore = "needs Docker or Podman and the debian:stable-slim image"]
fn reports_container_output() {
    let mut process =
        ContainerProcess::start(&spec("ends", "read line; echo gone >&2; exit 3")).expect("starts");
    let error = process.exchange("a").expect_err("no reply");
    assert!(matches!(error, ProcessError::Ended { .. }), "{error}");
    assert!(error.to_string().contains("gone"), "{error}");
}

#[test]
#[ignore = "needs Docker or Podman and the debian:stable-slim image"]
fn waits_container_completion() {
    run_to_end(&spec("done", "true")).expect("runs");
    let error = run_to_end(&spec("fails", "echo broken >&2; exit 2")).expect_err("fails");
    assert!(error.to_string().contains("broken"), "{error}");
}

#[test]
#[ignore = "needs Docker or Podman and the debian:stable-slim image"]
fn cancellation_removes_helper() {
    let settings = spec("hanging-cancel", "read line; sleep 300");
    let cancel = executor::CancellationToken::new();
    let mut process = ContainerProcess::start_cancellable(&settings, &cancel).expect("starts");
    let trigger = cancel.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        trigger.cancel();
    });
    let started = std::time::Instant::now();
    assert!(matches!(
        process.exchange("hang"),
        Err(ProcessError::Cancelled)
    ));
    assert!(started.elapsed() < std::time::Duration::from_secs(12));
    canceller.join().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let engine = runtime.block_on(executor::ContainerEngine::find()).unwrap();
    let error = runtime
        .block_on(engine.client().inspect_container(&settings.name, None))
        .unwrap_err();
    assert!(matches!(
        error,
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            ..
        }
    ));
}

#[test]
#[ignore = "needs Docker/Podman and a Python helper image with packaging or pip vendored packaging"]
fn constrains_package_installs() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let folder = tempfile::tempdir().unwrap();
    for (name, body) in [
        ("a", "import b\nVERSION = b.VERSION\n"),
        ("b", "VERSION = 2\n"),
    ] {
        std::fs::write(folder.path().join(format!("{name}.py")), body).unwrap();
        let metadata = folder.path().join(format!("{name}-2.dist-info"));
        std::fs::create_dir(&metadata).unwrap();
        std::fs::write(
            metadata.join("METADATA"),
            format!("Metadata-Version: 2.1\nName: {name}\nVersion: 2\nRequires-Dist: b>=2\n"),
        )
        .unwrap();
    }
    let mut settings = spec("package-pins", "");
    let owner = std::fs::metadata(folder.path()).unwrap();
    settings.user = Some(format!("{}:{}", owner.uid(), owner.gid()));
    settings.image = std::env::var("UWE_TEST_PYTHON_IMAGE")
        .unwrap_or_else(|_| "ghcr.io/astral-sh/uv:0.9.30-python3.12-bookworm-slim".into());
    settings.entrypoint = Some("python3".into());
    settings.command = vec![
        "-u".into(),
        "-c".into(),
        include_str!("../../python-evaluator/src/packages/server.py").into(),
    ];
    settings.mounts = vec![executor::process::Mount {
        source: folder.path().to_owned(),
        target: "/fixture".into(),
        read_only: false,
    }];
    settings.environment = vec![
        ("PYTHONPATH".into(), "/fixture".into()),
        ("UWE_PYTHON_CONSTRAINTS".into(), r#"["b==1"]"#.into()),
    ];
    let mut process = ContainerProcess::start(&settings).unwrap();
    for (module, packages) in [
        ("a", serde_json::json!(["a"])),
        ("b", serde_json::json!(["b==1"])),
    ] {
        let request = serde_json::json!({"op": "import", "module": module, "packages": packages});
        let reply: serde_json::Value =
            serde_json::from_str(&process.exchange(&request.to_string()).unwrap()).unwrap();
        assert_eq!(reply["error"]["type"], "ImportError");
        assert!(
            reply["error"]["message"]
                .as_str()
                .unwrap()
                .contains("installed b==2")
        );
    }
    process.stop();
    std::fs::write(folder.path().join("uv"), "#!/bin/sh\nwhile [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = --constraint ]; then cat \"$2\" >> /fixture/attempts; cat \"$2\" >&2; exit 1; fi\n  shift\ndone\necho missing-constraint >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(
        folder.path().join("uv"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    settings.name.push_str("-later");
    settings.environment = vec![
        ("PYTHONPATH".into(), "/fixture".into()),
        (
            "PATH".into(),
            "/fixture:/usr/local/bin:/usr/bin:/bin".into(),
        ),
        ("UWE_PYTHON_CONSTRAINTS".into(), r#"["c==1"]"#.into()),
    ];
    let mut process = ContainerProcess::start(&settings).unwrap();
    let request = serde_json::json!({"op": "import", "module": "c", "packages": ["c==1"]});
    let reply: serde_json::Value =
        serde_json::from_str(&process.exchange(&request.to_string()).unwrap()).unwrap();
    assert_eq!(reply["error"]["type"], "ModuleNotFoundError");
    assert!(reply["error"]["message"].as_str().unwrap().contains("c==1"));
    assert_eq!(
        std::fs::read_to_string(folder.path().join("attempts")).unwrap(),
        "c==1\nc==1\n"
    );
    process.stop();
    settings.name.push_str("-no-validator");
    settings.command[2] = format!(
        "import builtins\noriginal_import = builtins.__import__\ndef without_validator(name, *args, **kwargs):\n    if name in ('packaging.requirements', 'pip._vendor.packaging.requirements'):\n        raise ModuleNotFoundError(name)\n    return original_import(name, *args, **kwargs)\nbuiltins.__import__ = without_validator\n{}",
        include_str!("../../python-evaluator/src/packages/server.py")
    );
    let mut process = ContainerProcess::start(&settings).unwrap();
    let reply: serde_json::Value =
        serde_json::from_str(&process.exchange(&request.to_string()).unwrap()).unwrap();
    assert_eq!(reply["error"]["type"], "ImportError");
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("requires packaging")
    );
    process.stop();
}
