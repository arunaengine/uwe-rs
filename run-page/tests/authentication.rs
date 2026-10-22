//! Authentication applies to every route of a remotely bound run page.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use run_page::{PageError, Server, Settings};

#[test]
fn redacts_settings_token() {
    let mut settings = Settings::new("runs.sqlite".into());
    settings.token = Some("private-token".into());
    let output = format!("{settings:?}");
    assert!(!output.contains("private-token"));
    assert!(output.contains("[redacted]"));
}

#[test]
fn requires_remote_token() {
    let mut settings = Settings::new("unused.sqlite".into());
    settings.address = "0.0.0.0".parse().unwrap();
    assert!(matches!(
        Server::start(&settings),
        Err(PageError::TokenRequired)
    ));
}

#[test]
fn requires_hosted_token() {
    let settings = Settings::new("unused.sqlite".into());
    assert!(matches!(
        Server::start_hosted(&settings, Default::default()),
        Err(PageError::TokenRequired)
    ));
}

#[test]
fn isolates_hosted_runs() {
    use run_database::{Database, RunStart};
    use workflow_graph::{Language, Outcome, Task, TaskId};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("runs.sqlite");
    let log = root.path().join("stdout");
    std::fs::write(&log, "retained stdout").unwrap();
    for id in ["foreign", "allowed", "foreign-replay"] {
        let start = RunStart {
            id: id.into(),
            resumes: None,
            workflow: "/work/main.nf".into(),
            language: Language::Nextflow,
            revision: None,
            arguments: vec![],
            parameters: vec![],
            executor: "local".into(),
            simulated: false,
            store: None,
            program: None,
            launch: None,
            workflow_folder: None,
            repeats: None,
        };
        let mut recorder = Database::open(&path).unwrap().record(&start).unwrap();
        if id == "foreign-replay" {
            recorder.replays("allowed", 1);
        }
        let task = Task {
            id: TaskId(1),
            step: "STEP".into(),
            ..Task::default()
        };
        recorder.requested(&task, &[]);
        recorder.started(task.id, 1);
        recorder.finished(
            &Outcome {
                id: task.id,
                stdout_log: Some(workflow_graph::Stream {
                    location: log.to_string_lossy().into_owned(),
                    digest: "sha256:retained".into(),
                    size: 15,
                }),
                ..Outcome::default()
            },
            1,
            false,
        );
        recorder.ended(None);
        recorder.close().unwrap();
    }
    let mut settings = Settings::new(path);
    settings.token = Some("tenant-token".into());
    let server = Server::start_hosted(&settings, ["allowed".into()].into()).unwrap();
    let client = reqwest::blocking::Client::new();
    let get = |route: &str| {
        client
            .get(format!("{}{route}", server.url()))
            .bearer_auth("tenant-token")
            .send()
            .unwrap()
    };
    let listed: serde_json::Value = get("/api/runs").json().unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["id"], "allowed");
    assert_eq!(
        get("/api/runs/allowed/tasks/1/logs/1/stdout")
            .text()
            .unwrap(),
        "retained stdout"
    );
    for route in [
        "/api/runs/foreign",
        "/api/runs/%66oreign/events",
        "/api/runs/foreign/time",
        "/api/runs/foreign/graph.svg",
        "/api/runs/foreign/tasks/1",
        "/api/runs/foreign/tasks/1/why",
        "/api/runs/foreign/tasks/1/logs/1/stdout",
        "/api/runs/foreign/tasks/1/outputs/0",
        "/api/compare/allowed/foreign",
        "/api/compare/foreign/allowed",
    ] {
        assert_eq!(get(route).status(), 404, "{route}");
    }
    let task: serde_json::Value = get("/api/runs/allowed/tasks/1").json().unwrap();
    assert_eq!(task["replays"], serde_json::json!([]));
    assert_eq!(
        get("/api/runs/allowed/tasks/1/why")
            .json::<serde_json::Value>()
            .unwrap(),
        serde_json::Value::Null
    );
    server.stop();
}

#[test]
fn authenticates_all_routes() {
    let root = tempfile::tempdir().unwrap();
    let mut settings = Settings::new(root.path().join("runs.sqlite"));
    settings.token = Some("test-token".into());
    let server = Server::start(&settings).unwrap();
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    for route in [
        "/",
        "/page.js",
        "/api/runs",
        "/api/runs/missing/events",
        "/api/runs/missing/tasks/1/output/0",
    ] {
        let url = format!("{}{route}", server.url());
        assert_eq!(client.get(&url).send().unwrap().status(), 401);
        assert_eq!(
            client
                .get(&url)
                .bearer_auth("wrong")
                .send()
                .unwrap()
                .status(),
            401
        );
        assert_ne!(
            client
                .get(&url)
                .bearer_auth("test-token")
                .send()
                .unwrap()
                .status(),
            401
        );
    }
    let response = client
        .get(format!("{}/?token=test-token", server.url()))
        .send()
        .unwrap();
    assert_eq!(response.status(), 303);
    assert_eq!(response.headers()["location"], "/");
    let cookie = response.headers()["set-cookie"].to_str().unwrap();
    assert!(cookie.contains("HttpOnly; SameSite=Strict"));
    assert_eq!(
        client
            .get(format!("{}/api/runs", server.url()))
            .header("cookie", cookie.split(';').next().unwrap())
            .send()
            .unwrap()
            .status(),
        200
    );
    server.stop();
}
