//! Frozen native jobs fixtures: Aruna 51c43dba96198542addb53d3375579c308de0b17.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{BufRead, Read, Write};
use std::thread::JoinHandle;
use std::time::Duration;

use aruna_executor::client::{Client, Error, Family, Input, InputMode, Output, Status, Submit};
use serde_json::{Value, json};

const ID: &str = "01JJRSTVWXYZ0123456789ABCD";

fn server(status: u16, body: String, headers: &str) -> (Client, JoinHandle<String>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/api/v1/", listener.local_addr().unwrap());
    let response = format!("HTTP/1.1 {status} Fixture\r\nConnection: close\r\n{headers}\r\n{body}");
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut reader = std::io::BufReader::new(&mut stream);
        let mut request = String::new();
        let mut length = 0;
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            request.push_str(&line);
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = value.trim().parse().unwrap();
            }
        }
        let mut bytes = vec![0; length];
        reader.read_exact(&mut bytes).unwrap();
        request.push_str(std::str::from_utf8(&bytes).unwrap());
        let _ = stream.write_all(response.as_bytes());
        request
    });
    (
        Client::new(&endpoint, Some("fixture-secret".into())).unwrap(),
        worker,
    )
}

fn status() -> Value {
    json!({"job_id": ID, "kind": "execution", "state": "succeeded", "attempts": 1,
        "cancel_requested": true, "created_at": "2026-04-09T14:23:11Z",
        "updated_at": "2026-04-09T14:31:47Z", "progress": {"current": 5, "unit": "phases"},
        "workspace_mode": "none", "locally_exhausted": true,
        "family": {"submission_id": "submission", "request_digest": "request",
            "canonical_job_id": "01JJRSTVWXYZ0123456789ABCE", "aliases": [ID], "alias_count": 1,
            "conflict_count": 0, "logical_state": "succeeded", "canonical_execution_id": "execution",
            "executions": 1, "execution_list": [{"execution_id": "execution", "executor_node_id": "node",
                "state": "succeeded", "canonical": true}], "duplicate_successes": 0,
            "outputs": [{"bucket": "results", "key": "out", "version_id": "version",
                "execution_id": "execution", "container_path": "/work/out", "size": 7,
                "endpoint_url": "https://output-owner.example/s3"}], "revision": 2,
            "projection_digest": "projection", "partial": true, "locally_exhausted": true,
            "cancel_requested": true}})
}

#[tokio::test]
async fn native_submission_sends_exact_references_resources_and_body_idempotency() {
    let body = json!({"job_id": ID, "created": false, "submission_id": "submission",
        "canonical_job_id": "01JJRSTVWXYZ0123456789ABCE", "state": "running",
        "origin_node_url": "https://other.example", "status_url": "https://other.example/status"});
    let (client, worker) = server(200, body.to_string(), "");
    assert!(!format!("{client:?}").contains("fixture-secret"));
    let request = Submit {
        group_id: ID.into(),
        image: format!("image@sha256:{}", "a".repeat(64)),
        name: None,
        entrypoint: Some(vec!["sh".into()]),
        command: vec!["/wrapper.sh".into()],
        env: Default::default(),
        workdir: Some("/work".into()),
        cpu_cores: Some(2),
        ram_bytes: Some(1_073_741_824),
        max_walltime_ms: Some(60_000),
        executor_constraint: None,
        tags: Default::default(),
        idempotency_key: "attempt-1".into(),
        inputs: vec![Input {
            bucket: "inputs".into(),
            key: "a?b".into(),
            version_id: "version".into(),
            dest_key: "input".into(),
            container_path: "/inputs/input".into(),
            mode: InputMode::ExactReference,
        }],
        outputs: vec![Output {
            bucket: "results".into(),
            dest_key: "out".into(),
            container_path: "/work/out".into(),
        }],
    };
    let submitted = client.submit(&request).await.unwrap();
    assert!(!submitted.created);
    assert_eq!(submitted.job_id, ID);
    assert_ne!(submitted.job_id, submitted.canonical_job_id);
    assert!(client.server().starts_with("http://127.0.0.1:"));
    let wire = worker.join().unwrap();
    assert!(wire.starts_with("POST /api/v1/compute/jobs HTTP/1.1\r\n"));
    assert!(
        wire.to_lowercase()
            .contains("authorization: bearer fixture-secret\r\n")
    );
    let body: Value = serde_json::from_str(wire.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(body["inputs"][0]["mode"], "exact_reference");
    assert_eq!(body["inputs"][0]["version_id"], "version");
    assert_eq!(body["inputs"][0]["key"], "a?b");
    assert_eq!(body["idempotency_key"], "attempt-1");
    assert_eq!(body["cpu_cores"], 2);
    assert_eq!(body["ram_bytes"], 1_073_741_824u64);
    assert_eq!(body["max_walltime_ms"], 60_000);
}

#[tokio::test]
async fn partial_status_and_cancel_late_success_remain_structured_evidence() {
    for (cancel, code, state) in [
        (false, 200, "succeeded"),
        (true, 202, "running"),
        (true, 200, "succeeded"),
    ] {
        let mut body = status();
        body["state"] = json!(state);
        if cancel {
            body.as_object_mut().unwrap().remove("family");
        }
        let (client, worker) = server(code, body.to_string(), "");
        let found = if cancel {
            client.cancel(ID).await
        } else {
            client.status(ID).await
        }
        .unwrap();
        assert_eq!(found.state, state);
        assert!(found.cancel_requested && found.locally_exhausted);
        if !cancel {
            let family = found.family.unwrap();
            assert!(family.partial && family.locally_exhausted);
            assert_eq!(family.outputs[0].version_id, "version");
            assert_eq!(
                family.outputs[0].endpoint_url.as_deref(),
                Some("https://output-owner.example/s3")
            );
        }
        let method = if cancel { "POST" } else { "GET" };
        let suffix = if cancel { "/cancel" } else { "" };
        assert!(
            worker
                .join()
                .unwrap()
                .starts_with(&format!("{method} /api/v1/compute/jobs/{ID}{suffix} "))
        );
    }
    let mut family = status()["family"].clone();
    family.as_object_mut().unwrap().remove("partial");
    assert!(serde_json::from_value::<Family>(family).is_err());
    assert!(serde_json::from_value::<Status>(json!({"state": "succeeded"})).is_err());
}

#[tokio::test]
async fn rejects_redirects_conflicts_oversized_bodies_and_unsafe_endpoints() {
    for code in [302, 409] {
        let (client, worker) = server(
            code,
            "fixture-secret".into(),
            "Location: https://other.example/\r\n",
        );
        let error = client.status(ID).await.unwrap_err();
        assert!(matches!(error, Error::Status(found) if found == code));
        assert!(!format!("{error:?} {error}").contains("fixture-secret"));
        worker.join().unwrap();
    }
    let (client, worker) = server(200, String::new(), "Content-Length: 1048577\r\n");
    assert!(matches!(client.status(ID).await, Err(Error::BodyLimit)));
    worker.join().unwrap();
    let (client, worker) = server(200, "x".repeat(1048577), "");
    assert!(matches!(client.status(ID).await, Err(Error::BodyLimit)));
    worker.join().unwrap();
    for endpoint in [
        "http://remote.example",
        "https://user:secret@example.test",
        "https://example.test/?token=x",
        "https://example.test/#x",
    ] {
        assert!(Client::new(endpoint, Some("fixture-secret".into())).is_err());
    }
    let client = Client::new("https://example.test/api/v1", None).unwrap();
    assert!(matches!(
        client.status("../other").await,
        Err(Error::Configuration)
    ));
}
