//! Tests the pod plan of `uwe kuberun` and its launch against a fake Kubernetes API server.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::*;

fn arguments(target: &str) -> Arguments {
    Arguments {
        target: Some(PathBuf::from(target)),
        head_image: Some("registry.example.org/uwe:0.1.0".into()),
        ..Arguments::default()
    }
}

#[test]
fn plans_driver_pod() {
    let folder = tempfile::tempdir().expect("a folder");
    let config = folder.path().join("extra.config");
    std::fs::write(&config, "process.cpus = 2\n").expect("writes the config");
    let mut given = arguments("nf-core/demo");
    given.revision = Some("1.0.0".into());
    given.profiles = Some(vec!["test".into(), "docker".into()]);
    given.configs = vec![config];
    given.params = vec![("outdir".into(), "/data/results".into())];
    given.volume_mounts = vec!["shared:/data".into()];
    given.head_cpus = Some("2".into());
    let plan = plan(&given, "uwe-1", "lab").expect("a plan");
    let command: Vec<&str> = plan.command.iter().map(String::as_str).collect();
    assert_eq!(
        command,
        [
            "uwe",
            "run",
            "nf-core/demo",
            "-executor",
            "kubernetes",
            "-kubeconfig",
            "/etc/uwe/kubeconfig",
            "-kubernetes-namespace",
            "lab",
            "-c",
            "/etc/uwe/config-0",
            "-r",
            "1.0.0",
            "-profile",
            "test,docker",
            "--outdir",
            "/data/results",
        ]
    );
    assert_eq!(plan.files["config-0"], "process.cpus = 2\n");
    let site = "[executor.kubernetes.shared_volume]\nclaim = \"shared\"\npath = \"/data\"\n";
    assert_eq!(plan.files["config.toml"], site);
    assert!(plan.files["kubeconfig"].contains("serviceaccount/token"));
    let pod = pod(&plan);
    let spec = &pod["spec"];
    assert_eq!(spec["serviceAccountName"], "uwe-1");
    assert_eq!(spec["securityContext"]["runAsNonRoot"], true);
    let container = &spec["containers"][0];
    assert_eq!(container["securityContext"]["privileged"], false);
    assert_eq!(
        container["securityContext"]["allowPrivilegeEscalation"],
        false
    );
    assert_eq!(
        container["securityContext"]["capabilities"]["drop"],
        json!(["ALL"])
    );
    assert_eq!(container["workingDir"], "/data");
    assert_eq!(container["resources"]["requests"], json!({ "cpu": "2" }));
    let claim = &spec["volumes"][1]["persistentVolumeClaim"]["claimName"];
    assert_eq!(claim, "shared");
    let [_, role, binding] = access(&plan);
    let resources: Vec<&Json> = (role["rules"].as_array().expect("rules").iter())
        .map(|rule| &rule["resources"][0])
        .collect();
    assert_eq!(resources, ["jobs", "pods", "pods/log", "events"]);
    assert_eq!(binding["subjects"][0]["name"], "uwe-1");
}

#[test]
fn refuses_local_workflows() {
    let folder = tempfile::tempdir().expect("a folder");
    let local = folder.path().join("main.nf");
    std::fs::write(&local, "workflow {}").expect("writes the script");
    let refused = |given: &Arguments| plan(given, "uwe-1", "lab").err().map(|error| error.message);
    let script = refused(&arguments(&local.to_string_lossy())).unwrap_or_default();
    assert!(
        script.starts_with("The k8s executor cannot run local script:"),
        "{script}"
    );
    let project = refused(&arguments(&folder.path().to_string_lossy())).unwrap_or_default();
    assert!(project.starts_with("The k8s executor cannot run local project:"));
    assert!(refused(&arguments("main.nf")).is_some());
    assert_eq!(
        plan(&arguments("/cluster/pipeline"), "u", "lab")
            .map(|_| ())
            .ok(),
        Some(())
    );
    let missing = Arguments::default();
    assert_eq!(
        refused(&missing).as_deref(),
        Some("No project name was specified")
    );
    let mut image = arguments("nf-core/demo");
    image.head_image = None;
    assert!(refused(&image).is_some_and(|error| error.contains("-head-image")));
    let mut mount = arguments("nf-core/demo");
    mount.volume_mounts = vec!["shared".into()];
    assert!(refused(&mount).is_some_and(|error| error.contains("-volume-mount")));
}

/// A request that the fake heard: method and path without the query, and the body.
type Heard = Arc<Mutex<Vec<(String, String)>>>;

/// Answers as the API server: echoes made objects, gives the pod as ended with `code`, and
/// two log lines.
fn api(code: i32) -> (String, Heard) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let base = format!("http://{}", listener.local_addr().expect("an address"));
    let heard: Heard = Arc::new(Mutex::new(Vec::new()));
    let log = heard.clone();
    std::thread::spawn(move || {
        let mut objects: HashMap<String, Json> = HashMap::new();
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            let mut length = 0;
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
                    break;
                }
                let header = header.to_ascii_lowercase();
                if let Some(value) = header.strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; length];
            let _ = reader.read_exact(&mut body);
            let body = String::from_utf8_lossy(&body).into_owned();
            let mut parts = line.split_whitespace();
            let method = parts.next().unwrap_or_default().to_owned();
            let target = parts.next().unwrap_or_default();
            let path = target.split('?').next().unwrap_or_default().to_owned();
            log.lock()
                .expect("the log")
                .push((format!("{method} {path}"), body.clone()));
            let (kind, reply) = match method.as_str() {
                "POST" => {
                    let mut object: Json = serde_json::from_str(&body).unwrap_or_default();
                    object["metadata"]["uid"] = json!("uid-1");
                    let name = object["metadata"]["name"].as_str().unwrap_or_default();
                    objects.insert(format!("{path}/{name}"), object.clone());
                    ("application/json", object.to_string())
                }
                _ if path.ends_with("/log") => ("text/plain", "hello\nworld\n".to_owned()),
                "GET" if path.contains("/pods/") => {
                    let mut pod = objects.get(&path).cloned().unwrap_or_default();
                    pod["status"] = json!({ "phase": "Succeeded", "containerStatuses": [{
                        "name": "uwe", "image": "i", "imageID": "", "ready": false,
                        "restartCount": 0, "state": { "terminated": { "exitCode": code } },
                    }]});
                    ("application/json", pod.to_string())
                }
                _ => {
                    let object = objects.get(&path).cloned().unwrap_or_default();
                    ("application/json", object.to_string())
                }
            };
            let answer = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            let _ = (&stream).write_all(answer.as_bytes());
        }
    });
    (base, heard)
}

fn launched(code: i32) -> (Result<u8, String>, Vec<(String, String)>) {
    let (base, heard) = api(code);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let result = runtime.block_on(async {
        let config = kube::Config::new(base.parse().expect("an address"));
        let client = Client::try_from(config).expect("a client");
        let cluster = Cluster::new(client, "lab".into(), base.clone());
        let plan = plan(&arguments("nf-core/demo"), "uwe-1", "lab").expect("a plan");
        let timing = Timing {
            start: Duration::ZERO,
            stop: Duration::ZERO,
            limit: Duration::from_secs(5),
        };
        launch(&cluster, &plan, false, &timing).await
    });
    let heard = heard.lock().expect("the log").clone();
    (result, heard)
}

#[test]
fn launches_driver_pod() {
    let (result, heard) = launched(0);
    assert_eq!(result, Ok(0));
    let requests: Vec<&str> = heard.iter().map(|(request, _)| request.as_str()).collect();
    let rbac = "/apis/rbac.authorization.k8s.io/v1/namespaces/lab";
    let core = "/api/v1/namespaces/lab";
    let made = [
        format!("POST {core}/serviceaccounts"),
        format!("POST {rbac}/roles"),
        format!("POST {rbac}/rolebindings"),
        format!("POST {core}/configmaps"),
        format!("POST {core}/pods"),
        format!("PATCH {core}/serviceaccounts/uwe-1"),
        format!("PATCH {rbac}/roles/uwe-1"),
        format!("PATCH {rbac}/rolebindings/uwe-1"),
        format!("PATCH {core}/configmaps/uwe-1"),
        format!("GET {core}/pods/uwe-1"),
        format!("GET {core}/pods/uwe-1/log"),
        format!("GET {core}/pods/uwe-1"),
        format!("DELETE {core}/pods/uwe-1"),
    ];
    assert_eq!(
        requests,
        made.iter().map(String::as_str).collect::<Vec<_>>()
    );
    let owner: Json = serde_json::from_str(&heard[5].1).expect("a patch");
    assert_eq!(owner["metadata"]["ownerReferences"][0]["uid"], "uid-1");
    // A failed driver keeps its pod.
    let (result, heard) = launched(3);
    assert_eq!(result, Ok(3));
    assert!(
        !heard
            .iter()
            .any(|(request, _)| request.starts_with("DELETE"))
    );
}
