//! Tests nf-wave with the request bodies that nf-wave 1.21.0 sent to a fake Wave service.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use nextflow_evaluator::value::Value;
use serde_json::{Value as Json, json};

use super::*;

/// A request that the fake heard: its first line, its headers in lower case and its body.
struct Heard {
    line: String,
    headers: Vec<String>,
    body: String,
}

/// Answers each request with the next of `replies` (status and body), the last one again
/// when they run out.
fn service(replies: Vec<(u16, String)>) -> (String, Arc<Mutex<Vec<Heard>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let base = format!("http://{}", listener.local_addr().expect("an address"));
    let heard = Arc::new(Mutex::new(Vec::new()));
    let log = heard.clone();
    std::thread::spawn(move || {
        for (index, stream) in listener.incoming().flatten().enumerate() {
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            let mut headers = Vec::new();
            let mut length = 0;
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
                    break;
                }
                let header = header.trim().to_ascii_lowercase();
                if let Some(value) = header.strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
                headers.push(header);
            }
            let mut body = vec![0; length];
            let _ = reader.read_exact(&mut body);
            log.lock().expect("the log").push(Heard {
                line: line.trim().to_owned(),
                headers,
                body: String::from_utf8_lossy(&body).into_owned(),
            });
            let (status, reply) = &replies[index.min(replies.len() - 1)];
            let answer = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            let _ = (&stream).write_all(answer.as_bytes());
        }
    });
    (base, heard)
}

fn config(text: &str) -> Config {
    let directory = tempfile::tempdir().expect("a folder");
    let file = directory.path().join("nextflow.config");
    std::fs::write(&file, text).expect("writes the config");
    let options = nextflow_config::Options {
        profiles: None,
        params: Value::map([]),
        project_dir: "/project".into(),
        launch_dir: "/launch".into(),
        work_dir: "/work".into(),
    };
    nextflow_config::load(&[file], &options, &mut Quiet).expect("loads")
}

struct Quiet;

impl nextflow_evaluator::interpreter::Host for Quiet {
    fn print(&mut self, _: &str) {}
}

fn sources(process: &str, container: Option<&str>, conda: Option<&str>) -> Sources {
    Sources {
        process: process.into(),
        container: container.map(str::to_owned),
        conda: conda.map(str::to_owned),
        platform: None,
        channels: vec!["conda-forge".into(), "bioconda".into()],
    }
}

/// A request body without the fields that change with each request.
fn stable(mut body: Json) -> Json {
    let fields = body.as_object_mut().expect("an object");
    assert_eq!(fields["fingerprint"].as_str().map(str::len), Some(32));
    assert!(
        fields["timestamp"]
            .as_str()
            .is_some_and(|time| time.contains('T'))
    );
    fields.remove("fingerprint");
    fields.remove("timestamp");
    body
}

/// The body of the recording with these fields changed.
fn recorded(changes: Json) -> Json {
    let mut body = json!({
        "buildCompression": null, "buildRepository": null, "buildTemplate": null,
        "cacheRepository": null,
        "containerConfig": {
            "cmd": null, "entrypoint": null, "env": null, "layers": null, "workingDir": null
        },
        "containerFile": null, "containerImage": null, "containerIncludes": null,
        "containerPlatform": "linux/amd64", "dryRun": false, "format": null, "freeze": false,
        "mirror": false, "nameStrategy": null, "packages": null, "scanLevels": null,
        "scanMode": null, "towerAccessToken": null, "towerEndpoint": "https://api.cloud.seqera.io",
        "towerRefreshToken": null, "towerWorkspaceId": null, "workflowId": null
    });
    for (key, value) in changes.as_object().expect("an object") {
        body[key] = value.clone();
    }
    body
}

fn conda_options() -> Json {
    json!({
        "baseImage": "ubuntu:24.04", "basePackages": "conda-forge::procps-ng",
        "commands": null, "mambaImage": "mambaorg/micromamba:1.5.10-noble"
    })
}

fn request(wave: &Wave, sources: &Sources) -> Json {
    let body = wave.request(sources).expect("a request");
    stable(body.expect("a body"))
}

#[test]
fn builds_recorded_bodies() {
    let text = "wave {\n enabled = true\n endpoint = 'http://127.0.0.1:1'\n}";
    let none = |_: &str| None;
    let wave = wave(&config(text), &none, true)
        .expect("settings")
        .expect("on");
    assert_eq!(wave.note(), None);
    let image = sources("WITH_IMAGE", Some("ubuntu:22.04"), None);
    let expected = recorded(json!({ "containerImage": "ubuntu:22.04", "dryRun": true }));
    assert_eq!(request(&wave, &image), expected);
    let packages = "bioconda::samtools=1.17 conda-forge::pigz=2.8";
    let conda = sources("WITH_CONDA", None, Some(packages));
    let packages = json!({
        "channels": ["conda-forge", "bioconda"], "condaOpts": conda_options(),
        "cranOpts": null, "entries": ["bioconda::samtools=1.17", "conda-forge::pigz=2.8"],
        "environment": null, "pixiOpts": null, "type": "CONDA"
    });
    let expected = recorded(json!({ "dryRun": true, "packages": packages }));
    assert_eq!(request(&wave, &conda), expected);
    assert_eq!(wave.request(&sources("NONE", None, None)), Ok(None));
}

#[test]
fn builds_freeze_bodies() {
    let text = "wave {\n enabled = true\n freeze = true\n strategy = 'conda,container'\n \
                build {\n  repository = 'registry.example.org/team/repo'\n  \
                cacheRepository = 'registry.example.org/team/cache'\n  \
                template = 'conda/pixi:v1'\n  compression {\n   mode = 'zstd'\n   level = 3\n  }\n \
                }\n scan {\n  mode = 'required'\n  allowedLevels = 'low,medium'\n }\n}";
    let folder = tempfile::tempdir().expect("a folder");
    let file = folder.path().join("env.yml");
    let environment = "name: probe\nchannels:\n  - conda-forge\n  - bioconda\n\
                       dependencies:\n  - samtools=1.17\n";
    std::fs::write(&file, environment).expect("writes the file");
    let none = |_: &str| None;
    let wave = wave(&config(text), &none, false)
        .expect("settings")
        .expect("on");
    let both = sources(
        "WITH_BOTH",
        Some("ubuntu:22.04"),
        Some("bioconda::samtools=1.17"),
    );
    let file = sources("WITH_FILE", None, Some(file.to_str().expect("a path")));
    let shared = json!({
        "buildCompression": { "force": null, "level": 3, "mode": "zstd" },
        "buildRepository": "registry.example.org/team/repo", "buildTemplate": "conda/pixi:v1",
        "cacheRepository": "registry.example.org/team/cache", "freeze": true,
        "scanLevels": ["LOW", "MEDIUM"], "scanMode": "required"
    });
    let with = |packages: Json| {
        let mut changes = shared.clone();
        changes["packages"] = packages;
        recorded(changes)
    };
    let listed = json!({
        "channels": ["conda-forge", "bioconda"], "condaOpts": conda_options(), "cranOpts": null,
        "entries": ["bioconda::samtools=1.17"], "environment": null, "pixiOpts": null,
        "type": "CONDA"
    });
    assert_eq!(request(&wave, &both), with(listed));
    let encoded = "bmFtZTogcHJvYmUKY2hhbm5lbHM6CiAgLSBjb25kYS1mb3JnZQogIC0gYmlvY29uZGEKZGVwZW5k\
                   ZW5jaWVzOgogIC0gc2FtdG9vbHM9MS4xNwo=";
    let file_packages = json!({
        "channels": null, "condaOpts": conda_options(), "cranOpts": null, "entries": null,
        "environment": encoded, "pixiOpts": null, "type": "CONDA"
    });
    assert_eq!(request(&wave, &file), with(file_packages));
}

#[test]
fn keeps_local_images() {
    let none = |_: &str| None;
    let local = wave(&config("wave.enabled = true"), &none, false).expect("settings");
    assert!(local.is_some_and(|local| local.note().is_some()));
    let off = |name: &str| (name == "NXF_DISABLE_WAVE_SERVICE").then(|| "1".to_owned());
    assert!(wave(&config("wave.enabled = true"), &off, false).is_ok_and(|wave| wave.is_none()));
    assert!(wave(&config("wave.freeze = true"), &none, false).is_ok_and(|wave| wave.is_none()));
    let token = |name: &str| (name == "TOWER_ACCESS_TOKEN").then(|| "secret".to_owned());
    let text = "wave { enabled = true; scan.mode = 'required' }\ntower.workspaceId = '42'";
    let wave = wave(&config(text), &token, false)
        .expect("settings")
        .expect("on");
    let body = request(&wave, &sources("P", Some("ubuntu:22.04"), None));
    assert_eq!(body["towerAccessToken"], "secret");
    assert_eq!(body["towerWorkspaceId"], 42);
}

#[test]
fn rejects_wrong_settings() {
    let none = |_: &str| None;
    let error = |text: &str| wave(&config(text), &none, false).err().unwrap_or_default();
    assert_eq!(
        error("wave { enabled = true; mirror = true; freeze = true }"),
        "Wave configuration setting 'wave.mirror' and 'wave.freeze' conflicts each other"
    );
    assert!(error("wave { enabled = true; mirror = true }").contains("requires the use of"));
    assert_eq!(
        error("wave { enabled = true; strategy = 'container,spack' }"),
        "Invalid value for 'wave.strategy' configuration attribute - offending value: spack"
    );
    assert_eq!(
        error("wave { enabled = true; build.repository = 'https://r.example/x' }"),
        "Config setting 'wave.build.repository' should not include any protocol prefix - \
         offending value: 'https://r.example/x'"
    );
    assert!(error("wave { enabled = true; endpoint = 'ftp://w' }").starts_with("Endpoint URL"));
    let text = "wave { enabled = true; mirror = true; build.repository = 'r.example/x' }";
    let mirror = wave(&config(text), &none, false)
        .expect("settings")
        .expect("on");
    let conda = mirror.request(&sources("P", None, Some("samtools")));
    assert_eq!(
        conda,
        Err("Invalid container mirror operation - missing source container".into())
    );
    let url = mirror.request(&sources("P", Some("docker://ubuntu"), None));
    assert!(url.is_err_and(|error| error.starts_with("Wave container request image cannot")));
    let mut arm = sources("P", Some("ubuntu:22.04"), None);
    arm.platform = Some("aarch64".into());
    let body = mirror.request(&arm).expect("a request").expect("a body");
    assert_eq!(body["containerPlatform"], "linux/arm64");
}

#[test]
fn sends_to_service() {
    let answer =
        json!({ "requestId": "req1", "targetImage": "wave.example/a:1", "succeeded": true });
    let (base, heard) = service(vec![(200, answer.to_string())]);
    let text =
        format!("wave {{ enabled = true; endpoint = '{base}/'; httpClient.maxRate = '100/sec' }}");
    let token = |name: &str| (name == "TOWER_ACCESS_TOKEN").then(|| "secret".to_owned());
    let mut wave = wave(&config(&text), &token, false)
        .expect("settings")
        .expect("on");
    let image = sources("P", Some("ubuntu:22.04"), None);
    assert_eq!(wave.resolve(&image), Ok(Some("wave.example/a:1".into())));
    // The same sources again reuse the answer.
    assert_eq!(wave.resolve(&image), Ok(Some("wave.example/a:1".into())));
    let heard = heard.lock().expect("the log");
    assert_eq!(heard.len(), 1);
    assert_eq!(heard[0].line, "POST /v1alpha2/container HTTP/1.1");
    let headers = heard[0].headers.join("\n");
    assert!(headers.contains("content-type: application/json"));
    assert!(headers.contains("authorization: bearer secret"));
    assert!(headers.contains("traceparent: 00-"));
    let body: Json = serde_json::from_str(&heard[0].body).expect("a JSON body");
    assert_eq!(body["containerImage"], "ubuntu:22.04");
}

#[test]
fn waits_for_build() {
    let pending =
        json!({ "requestId": "req9", "buildId": "bd-9", "targetImage": "wave.example/x:1" });
    let (base, heard) = service(vec![
        (200, pending.to_string()),
        (
            200,
            json!({ "id": "req9", "status": "PENDING" }).to_string(),
        ),
        (
            200,
            json!({ "id": "req9", "status": "DONE", "succeeded": false,
            "reason": "conda failed", "detailsUri": "https://wave.example/view/builds/bd-9" })
            .to_string(),
        ),
    ]);
    let text = format!(
        "wave.enabled = true\nwave.endpoint = '{base}'\nwave.httpClient.maxRate = '100/sec'"
    );
    let none = |_: &str| None;
    let mut wave = wave(&config(&text), &none, false)
        .expect("settings")
        .expect("on");
    wave.poll = Duration::ZERO;
    let failed = wave.resolve(&sources("P", Some("ubuntu:22.04"), None));
    assert_eq!(
        failed,
        Err(
            "Wave provisioning for container 'wave.example/x:1' did not complete successfully\n\
             - Reason: conda failed\n- Find out more here: https://wave.example/view/builds/bd-9"
                .into()
        )
    );
    let heard = heard.lock().expect("the log");
    let lines: Vec<&str> = heard.iter().map(|heard| heard.line.as_str()).collect();
    assert_eq!(
        lines,
        [
            "POST /v1alpha2/container HTTP/1.1",
            "GET /v1alpha2/container/req9/status HTTP/1.1",
            "GET /v1alpha2/container/req9/status HTTP/1.1",
        ]
    );
}

#[test]
fn reports_refused_requests() {
    let (base, _) = service(vec![(400, "{\"message\":\"bad image\"}".into())]);
    let text = format!("wave.enabled = true\nwave.endpoint = '{base}'");
    let none = |_: &str| None;
    let mut wave = wave(&config(&text), &none, false)
        .expect("settings")
        .expect("on");
    assert_eq!(
        wave.resolve(&sources("P", Some("ubuntu:22.04"), None)),
        Err(format!(
            "Wave invalid response: POST {base}/v1alpha2/container [400] {{\"message\":\"bad image\"}}"
        ))
    );
    let url = wave.resolve(&sources("P", Some("docker://ubuntu"), None));
    assert!(url.is_err_and(|error| error.starts_with("Wave container request image cannot start")));
}
