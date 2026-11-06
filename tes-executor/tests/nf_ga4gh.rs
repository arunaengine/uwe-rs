//! Checks the TES requests of a Nextflow `tes` config against a local fake and nf-ga4gh's: API
//! path, token, tags, resources, API keys and the fields of the Tencent Omics platform.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use executor::registry::ImageConfigs;
use executor::{Backend, Job, Staging};
use serde_json::{Value, json};
use task_model::{FORMAT, Image, Placement, TaskSpec};
use tes_executor::{Client, Platform, Settings, TesBackend};
use tokio_util::sync::CancellationToken;

/// One request that the fake got: its path with query, lower-case headers and body.
#[derive(Clone, Debug)]
struct Request {
    path: String,
    headers: BTreeMap<String, String>,
    body: Value,
}

/// A server that takes tasks and answers that they are complete, after it wrote the control
/// files of the wrapper to their outputs, and keeps every request.
fn serve(requests: Arc<Mutex<Vec<Request>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let address = format!("http://{}", listener.local_addr().expect("an address"));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut reader = BufReader::new(&mut stream);
            let mut first = String::new();
            reader.read_line(&mut first).expect("a request line");
            let mut headers = BTreeMap::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).expect("a header");
                if line.trim().is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
                }
            }
            let length = headers
                .get("content-length")
                .and_then(|text| text.parse().ok());
            let mut body = vec![0; length.unwrap_or(0)];
            reader.read_exact(&mut body).expect("a body");
            let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let mut words = first.split_whitespace();
            let (method, path) = (
                words.next().unwrap_or_default(),
                words.next().unwrap_or_default(),
            );
            let created = path.ends_with("/tasks") || path.contains("/tasks?");
            let answer = if method == "POST" && created {
                for output in body["outputs"].as_array().into_iter().flatten() {
                    leave(output);
                }
                json!({"id": "task-1"})
            } else {
                json!({"id": "task-1", "state": "COMPLETE"})
            };
            let path = path.to_owned();
            requests.lock().expect("the requests").push(Request {
                path,
                headers,
                body,
            });
            let answer = answer.to_string();
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{answer}",
                answer.len()
            );
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    address
}

/// Writes the control files of a task that ended with 0 to its control output.
fn leave(output: &Value) {
    if output["path"].as_str() != Some("/uwe/control") {
        return;
    }
    let url = output["url"].as_str().expect("a URL");
    let folder = PathBuf::from(url.strip_prefix("file://").expect("a file URL"));
    std::fs::create_dir_all(&folder).expect("makes the folder");
    std::fs::write(folder.join("exit"), "0\n").expect("writes");
    std::fs::write(folder.join("manifest"), "").expect("writes");
}

fn spec() -> TaskSpec {
    TaskSpec {
        run_as: None,
        format: FORMAT,
        namespace: String::new(),
        image: Some(Image {
            reference: "ubuntu:24.04".into(),
            platform: None,
        }),
        arguments: vec!["true".into()],
        workdir: None,
        environment: Default::default(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        output_manifest: None,
        stdin: None,
        containers: Vec::new(),
        replaces_entrypoint: true,
    }
}

/// Runs one task of `placement` with a client of `client` and `settings`, and gives the
/// requests that the server got.
fn run(
    client: Client,
    settings: Settings,
    placement: &Placement,
    requests: &Mutex<Vec<Request>>,
) -> Vec<Request> {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let backend = TesBackend {
        client,
        scratch: folder.path().join("scratch"),
        staging: None,
        settings,
        configs: ImageConfigs::default(),
    };
    let spec = spec();
    let staging = Staging::default();
    let job = Job {
        name: "run-ALIGN-1-1",
        spec: &spec,
        staging: &staging,
        placement,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let finished = runtime
        .block_on(backend.run(job, &CancellationToken::new()))
        .expect("runs");
    assert_eq!(finished.exit_code, Some(0));
    requests.lock().expect("the requests").clone()
}

/// The recorded requests of nf-ga4gh.
fn recorded() -> Vec<Value> {
    let text = include_str!("fixtures/nf_ga4gh_requests.jsonl");
    (text.lines())
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .collect()
}

/// A Nextflow endpoint gets `/v1` added, as nf-ga4gh adds it; the task carries the bearer
/// token, the tags and the CPUs as nf-ga4gh's, but memory and disk in GB of 10^9 bytes.
#[test]
fn matches_recorded_tasks() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let address = serve(requests.clone());
    let client = Client::new(&format!("{address}/v1"), Some("not-a-real-token".into()));
    let settings = Settings {
        tags: [("project", "demo"), ("owner", "lab")]
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .into(),
        ..Settings::default()
    };
    let placement = Placement {
        cpus: 2,
        memory_bytes: Some(3 << 30),
        disk_bytes: Some(10 << 30),
        time_seconds: Some(3600),
        ..Placement::default()
    };
    let sent = run(client.expect("a client"), settings, &placement, &requests);
    let theirs = recorded();
    let (ours, created) = (&sent[0], &theirs[0]);
    assert_eq!(ours.path, created["path"]);
    assert_eq!(ours.headers["authorization"], "Bearer not-a-real-token");
    assert_eq!(created["headers"]["authorization"], "Bearer <masked>");
    for (name, value) in created["body"]["tags"].as_object().expect("tags") {
        assert_eq!(&ours.body["tags"][name], value, "{name}");
    }
    let (resources, recorded) = (&ours.body["resources"], &created["body"]["resources"]);
    assert_eq!(resources["cpu_cores"], recorded["cpu_cores"]);
    // nf-ga4gh counts GiB; uwe counts GB of 10^9 bytes, as the TES specification says.
    assert_eq!(recorded["ram_gb"], 3.0);
    assert_eq!(resources["ram_gb"], (3u64 << 30) as f64 / 1e9);
    assert_eq!(resources["disk_gb"], (10u64 << 30) as f64 / 1e9);
    assert_eq!(
        ours.body["executors"][0]["image"],
        created["body"]["executors"][0]["image"]
    );
    assert!(ours.body["resources"].get("preemptible").is_none());
    let polled = sent[1..]
        .iter()
        .all(|request| request.path.starts_with("/v1/tasks/task-1"));
    assert!(polled, "{sent:?}");
}

/// An API key goes in the query or in a header, and the Tencent Omics platform gets its tags,
/// spot VMs and GPU parameters from resource labels.
#[test]
fn sends_platform_fields() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let address = serve(requests.clone());
    let client = Client::new(&format!("{address}/v1"), None).expect("a client");
    let client = client.with_api_key("key".into(), "not-a-real-key".into(), false);
    let settings = Settings {
        platform: Some(Platform::TencentOmics),
        ..Settings::default()
    };
    let labels = [
        ("preemptible", "true"),
        ("gpuType", "T4"),
        ("gpuCount", "1"),
    ];
    let placement = Placement {
        labels: labels
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .into(),
        ..Placement::default()
    };
    let sent = run(client, settings, &placement, &requests);
    assert!(
        sent.iter()
            .all(|request| request.path.contains("key=not-a-real-key")),
        "{sent:?}"
    );
    let body = &sent[0].body;
    assert_eq!(body["resources"]["preemptible"], true);
    let parameters = json!({"gpuType": "T4", "gpuCount": "1"});
    assert_eq!(body["resources"]["backend_parameters"], parameters);
    assert!(body["tags"]["WorkingDir"].is_string(), "{body}");
    assert_eq!(body["tags"]["DestDir"], body["outputs"][0]["url"]);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let address = serve(requests.clone());
    let client = Client::new(&address, None).expect("a client");
    let client = client.with_api_key("X-Api-Key".into(), "not-a-real-key".into(), true);
    let sent = run(
        client,
        Settings::default(),
        &Placement::default(),
        &requests,
    );
    assert!(
        sent[0].path.starts_with("/ga4gh/tes/v1/tasks"),
        "{}",
        sent[0].path
    );
    assert_eq!(sent[0].headers["x-api-key"], "not-a-real-key");
    assert!(!sent[0].path.contains("not-a-real-key"));
}
