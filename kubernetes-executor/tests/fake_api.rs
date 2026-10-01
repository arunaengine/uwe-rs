//! Checks the Kubernetes backend with a local API server that executes job commands.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use executor::registry::{ImageConfig, ImageConfigs};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use executor::{Backend, ExecutorError, Job, Staged, Staging};
use kubernetes_executor::{Cluster, KubernetesBackend, LABEL, Look, Pods};
use serde_json::{Value, json};
use task_model::{ContainerSpec, FORMAT, Image, OutputRule, Placement, TaskSpec};
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "iunctus-test-token";
const NAMESPACE: &str = "iunctus-test";
const JOBS: &str = "/apis/batch/v1/namespaces/iunctus-test/jobs";
const PODS: &str = "/api/v1/namespaces/iunctus-test/pods";

/// A Job that the fake server runs.
struct Running {
    label: String,
    process: u32,
    deleted: bool,
    /// How many containers its pod has.
    containers: usize,
}

/// What the fake server knows: its folder, the Jobs, and each request as `METHOD path`.
struct State {
    folder: PathBuf,
    jobs: BTreeMap<String, Running>,
    calls: Vec<String>,
    submitted: Vec<Value>,
}

fn request(stream: &mut TcpStream) -> Option<(String, String, Option<String>, String)> {
    let mut reader = BufReader::new(stream);
    let mut first = String::new();
    reader.read_line(&mut first).ok()?;
    let (mut length, mut authorization) = (0, None);
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        if line.trim().is_empty() {
            break;
        }
        let (name, value) = line.split_once(':')?;
        match name.to_ascii_lowercase().as_str() {
            "content-length" => length = value.trim().parse().unwrap_or(0),
            "authorization" => authorization = Some(value.trim().to_owned()),
            _ => {}
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    let mut words = first.split_whitespace();
    let method = words.next()?.to_owned();
    let target = words.next()?.to_owned();
    Some((
        method,
        target,
        authorization,
        String::from_utf8_lossy(&body).into_owned(),
    ))
}

fn reply(stream: &mut TcpStream, status: &str, kind: &str, body: &str) {
    let text = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(text.as_bytes());
}

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// Runs the containers of a Job at once as a process group of its own, in which each writes
/// its exit code.
fn start(folder: &Path, job: &Value) -> Running {
    let name = job["metadata"]["name"].as_str().unwrap_or_default();
    let containers =
        (job["spec"]["template"]["spec"]["containers"].as_array()).expect("containers");
    let mut script = String::new();
    for (index, container) in containers.iter().enumerate() {
        let words: Vec<String> = (container["command"].as_array().expect("a command").iter())
            .map(|word| quote(word.as_str().unwrap_or_default()))
            .collect();
        // An image without Bash cannot run the wrapper.
        let command = match container["image"].as_str() {
            Some("without-shell") => "(echo 'exec: \"sh\": not found' >&2; exit 127)".to_owned(),
            _ => words.join(" "),
        };
        let directory = container["workingDir"].as_str().unwrap_or("/");
        let exit = folder.join(format!("{name}.exit.{index}"));
        script.push_str(&format!(
            "(cd {} && {command}; echo $? > {}) &\n",
            quote(directory),
            quote(&exit.to_string_lossy())
        ));
    }
    script.push_str("wait\n");
    let log = std::fs::File::create(folder.join(format!("{name}.log"))).expect("a log");
    let mut child = std::process::Command::new("bash")
        .arg("-c")
        .arg(script)
        .stdout(log.try_clone().expect("the log"))
        .stderr(log)
        .process_group(0)
        .spawn()
        .expect("runs the Job");
    let process = child.id();
    // The process is reaped when it ends, as the kubelet would.
    std::thread::spawn(move || child.wait());
    Running {
        label: job["metadata"]["labels"][LABEL]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        process,
        deleted: false,
        containers: containers.len(),
    }
}

/// The pod of a Job, as the API lists it.
fn pod(folder: &Path, name: &str, running: &Running) -> Value {
    let states: Vec<Value> = (0..running.containers)
        .map(|index| {
            let exit = std::fs::read_to_string(folder.join(format!("{name}.exit.{index}"))).ok();
            match exit.and_then(|text| text.trim().parse::<i32>().ok()) {
                Some(code) => {
                    let reason = if code == 0 { "Completed" } else { "Error" };
                    json!({"terminated": {"exitCode": code, "reason": reason}})
                }
                None => json!({"running": {}}),
            }
        })
        .collect();
    let running_now = states.iter().any(|state| state.get("running").is_some());
    let statuses: Vec<Value> = (states.into_iter().enumerate())
        .map(|(index, state)| {
            json!({
                "name": format!("task-{index}"), "image": "image", "imageID": "", "ready": false,
                "restartCount": 0, "state": state,
            })
        })
        .collect();
    json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "name": format!("{name}-pod"),
            "namespace": NAMESPACE,
            "labels": {"job-name": name, LABEL: running.label},
        },
        "status": {
            "phase": if running_now { "Running" } else { "Succeeded" },
            "containerStatuses": statuses,
        },
    })
}

/// The value of a query parameter, decoded.
fn parameter(target: &str, name: &str) -> Option<String> {
    let query = target.split_once('?')?.1;
    let value = query
        .split('&')
        .find_map(|pair| pair.strip_prefix(&format!("{name}=")))?;
    let mut decoded = Vec::new();
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
                decoded.push(u8::from_str_radix(hex, 16).ok()?);
                index += 3;
            }
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(decoded).ok()
}

fn answer(state: &Mutex<State>, mut stream: TcpStream) {
    let Some((method, target, authorization, body)) = request(&mut stream) else {
        return;
    };
    let mut state = state.lock().expect("the state");
    let path = target.split('?').next().unwrap_or_default().to_owned();
    state.calls.push(format!("{method} {path}"));
    if path.starts_with("/v2/") {
        let value = match path.as_str() {
            "/v2/tool/manifests/1" => json!({"manifests": [
                {"digest": "sha256:aa", "platform": {"os": "linux", "architecture": "arm64"}},
                {"digest": "sha256:bb", "platform": {"os": "linux", "architecture": "amd64"}},
            ]}),
            "/v2/tool/manifests/sha256:aa" => json!({"config": {"digest": "sha256:cc"}}),
            "/v2/tool/blobs/sha256:cc" => json!({"os": "linux", "architecture": "arm64",
                "config": {"Entrypoint": ["env", "FROM_ENTRYPOINT=arm"], "Cmd": [], "User": "1000"}}),
            "/v2/secondary/manifests/1" => json!({"manifests": [
                {"digest": "sha256:dd", "platform": {"os": "linux", "architecture": "arm64"}},
                {"digest": "sha256:ee", "platform": {"os": "linux", "architecture": "amd64"}},
            ]}),
            "/v2/secondary/manifests/sha256:dd" => json!({"config": {"digest": "sha256:ff"}}),
            "/v2/secondary/manifests/sha256:ee" => json!({"config": {"digest": "sha256:gg"}}),
            "/v2/secondary/blobs/sha256:ff" => json!({"os": "linux", "architecture": "arm64",
                "config": {"Entrypoint": ["env", "SECONDARY_PLATFORM=arm"],
                    "Cmd": ["/bin/bash", "-c", "echo $SECONDARY_PLATFORM-arm-default > secondary.txt"]}}),
            "/v2/secondary/blobs/sha256:gg" => json!({"os": "linux", "architecture": "amd64",
                "config": {"Entrypoint": ["env", "SECONDARY_PLATFORM=amd"],
                    "Cmd": ["/bin/bash", "-c", "echo $SECONDARY_PLATFORM-amd-default > secondary.txt"]}}),
            _ => return reply(&mut stream, "404 Not Found", "application/json", "{}"),
        };
        return reply(
            &mut stream,
            "200 OK",
            "application/json",
            &value.to_string(),
        );
    }
    if authorization.as_deref() != Some(&format!("Bearer {TOKEN}")) {
        let status =
            json!({"kind": "Status", "apiVersion": "v1", "status": "Failure", "code": 401});
        return reply(
            &mut stream,
            "401 Unauthorized",
            "application/json",
            &status.to_string(),
        );
    }
    let folder = state.folder.clone();
    match (method.as_str(), path.as_str()) {
        ("POST", JOBS) => {
            let mut job: Value = serde_json::from_str(&body).expect("a Job");
            job["metadata"]["uid"] = json!(format!("accepted-{}", state.submitted.len()));
            state.submitted.push(job.clone());
            let name = job["metadata"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let running = start(&folder, &job);
            state.jobs.insert(name, running);
            if folder.join("lose-submit").exists() {
                return;
            }
            reply(
                &mut stream,
                "201 Created",
                "application/json",
                &job.to_string(),
            );
        }
        ("GET", job) if job.starts_with(&format!("{JOBS}/")) => {
            let name = job.rsplit('/').next().unwrap_or_default();
            if state.jobs.get(name).is_some_and(|running| !running.deleted) {
                let job = state
                    .submitted
                    .iter()
                    .find(|job| job["metadata"]["name"] == name)
                    .unwrap();
                return reply(&mut stream, "200 OK", "application/json", &job.to_string());
            }
            let status = json!({"kind":"Status","apiVersion":"v1","status":"Failure","code":404,
                "reason":"NotFound","message":format!("Job {name} was not found")});
            reply(
                &mut stream,
                "404 Not Found",
                "application/json",
                &status.to_string(),
            );
        }
        ("GET", PODS) => {
            let selector = parameter(&target, "labelSelector").unwrap_or_default();
            let (key, value) = selector.split_once('=').unwrap_or_default();
            let items: Vec<Value> = (state.jobs.iter())
                .filter(|(name, running)| {
                    let label = if key == "job-name" {
                        name.as_str()
                    } else {
                        &running.label
                    };
                    !running.deleted && label == value
                })
                .map(|(name, running)| pod(&folder, name, running))
                .collect();
            let list =
                json!({"apiVersion": "v1", "kind": "PodList", "metadata": {}, "items": items});
            reply(&mut stream, "200 OK", "application/json", &list.to_string());
        }
        ("GET", log) if log.starts_with(PODS) && log.ends_with("/log") => {
            let pod = log
                .trim_end_matches("/log")
                .rsplit('/')
                .next()
                .unwrap_or_default();
            let name = pod.trim_end_matches("-pod");
            let text = std::fs::read_to_string(folder.join(format!("{name}.log")));
            reply(
                &mut stream,
                "200 OK",
                "text/plain",
                &text.unwrap_or_default(),
            );
        }
        ("DELETE", job) if job.starts_with(JOBS) => {
            let name = job.rsplit('/').next().unwrap_or_default().to_owned();
            if let Some(running) = state.jobs.get_mut(&name) {
                running.deleted = !folder.join("delay-delete").exists();
                let _ = std::process::Command::new("kill")
                    .args(["-TERM", "--", &format!("-{}", running.process)])
                    .stderr(std::process::Stdio::null())
                    .status();
            }
            let status =
                json!({"kind": "Status", "apiVersion": "v1", "status": "Success", "metadata": {}});
            reply(
                &mut stream,
                "200 OK",
                "application/json",
                &status.to_string(),
            );
        }
        _ => {
            let status =
                json!({"kind": "Status", "apiVersion": "v1", "status": "Failure", "code": 404});
            reply(
                &mut stream,
                "404 Not Found",
                "application/json",
                &status.to_string(),
            );
        }
    }
}

/// Starts the fake server and writes a kubeconfig for it; gives the kubeconfig and the state.
fn serve(root: &Path) -> (PathBuf, Arc<Mutex<State>>) {
    let folder = root.join("server");
    std::fs::create_dir_all(&folder).expect("makes the folder");
    let state = Arc::new(Mutex::new(State {
        folder,
        jobs: BTreeMap::new(),
        calls: Vec::new(),
        submitted: Vec::new(),
    }));
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let address = listener.local_addr().expect("an address");
    let shared = state.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let state = shared.clone();
            std::thread::spawn(move || answer(&state, stream));
        }
    });
    let kubeconfig = root.join("kubeconfig");
    let text = format!(
        "apiVersion: v1\nkind: Config\ncurrent-context: test\nclusters:\n- name: test\n  \
         cluster:\n    server: http://{address}\ncontexts:\n- name: test\n  context:\n    \
         cluster: test\n    user: test\n    namespace: {NAMESPACE}\nusers:\n- name: test\n  \
         user:\n    token: {TOKEN}\n"
    );
    std::fs::write(&kubeconfig, text).expect("writes the kubeconfig");
    (kubeconfig, state)
}

async fn backend(root: &Path) -> (KubernetesBackend, Arc<Mutex<State>>) {
    let (kubeconfig, state) = serve(root);
    let cluster = Cluster::connect(&kubeconfig, None)
        .await
        .expect("a cluster");
    assert_eq!(cluster.namespace(), NAMESPACE);
    let mut backend = KubernetesBackend::new(cluster, root.join("scratch"));
    backend.poll = Duration::from_millis(100);
    // The images of the tests are made up, so no registry gives their configs; one runs its
    // commands through an entrypoint, as Biocontainers do.
    let entrypoint = ImageConfig {
        entrypoint: vec!["env".into(), "FROM_ENTRYPOINT=yes".into()],
        cmd: Vec::new(),
        user: String::new(),
    };
    backend.configs = ImageConfigs::known([
        ("debian:stable-slim".to_owned(), ImageConfig::default()),
        ("without-shell".to_owned(), ImageConfig::default()),
        ("with-entrypoint".to_owned(), entrypoint),
    ]);
    (backend, state)
}

fn spec(image: &str, script: &str) -> TaskSpec {
    TaskSpec {
        run_as: None,
        format: FORMAT,
        namespace: String::new(),
        image: Some(Image {
            reference: image.into(),
            platform: None,
        }),
        arguments: vec!["/bin/bash".into(), "-c".into(), script.into()],
        workdir: None,
        environment: BTreeMap::new(),
        inputs: Vec::new(),
        outputs: vec![OutputRule {
            pattern: "out.txt".into(),
        }],
        stdin: None,
        containers: Vec::new(),
        replaces_entrypoint: false,
    }
}

fn placement() -> Placement {
    Placement {
        cpus: 1,
        memory_bytes: Some(256 << 20),
        ..Placement::default()
    }
}

#[tokio::test]
async fn preserves_api_errors() {
    let root = tempfile::tempdir().expect("a folder");
    let (kubeconfig, _) = serve(root.path());
    let text = std::fs::read_to_string(&kubeconfig).expect("a kubeconfig");
    std::fs::write(&kubeconfig, text.replace(TOKEN, "wrong-token")).expect("writes");
    let cluster = Cluster::connect(&kubeconfig, None)
        .await
        .expect("a cluster");
    let pods = Pods::default();
    pods.add("task").await;
    let look = pods.look("task", Duration::ZERO, &cluster, "test").await;
    assert!(
        matches!(look, Look::Unavailable(ref error) if error.contains("401")),
        "{look:?}"
    );
}

#[tokio::test]
async fn missing_is_absent() {
    let root = tempfile::tempdir().unwrap();
    let (backend, _) = backend(root.path()).await;
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        backend.cluster.jobs().get_opt("missing"),
    )
    .await;
    assert!(matches!(result, Ok(Ok(None))), "{result:?}");
}

#[tokio::test]
async fn rejects_offline_tasks() {
    let root = tempfile::tempdir().expect("a folder");
    let (backend, state) = backend(root.path()).await;
    let spec = spec("debian:stable-slim", "true");
    let staging = Staging::default();
    let placement = Placement {
        offline: true,
        ..placement()
    };
    let job = Job {
        name: "offline",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let result = backend.run(job, &CancellationToken::new()).await;
    assert!(
        matches!(result, Err(ExecutorError::Unsupported(_))),
        "{result:?}"
    );
    assert!(state.lock().expect("the state").calls.is_empty());
    assert!(!backend.scratch.exists());
}

#[tokio::test]
async fn returns_job_outputs() {
    let root = tempfile::tempdir().expect("a folder");
    let (backend, state) = backend(root.path()).await;
    let source = root.path().join("source/in.txt");
    std::fs::create_dir_all(source.parent().expect("a folder")).expect("makes the folder");
    std::fs::write(&source, "hello\n").expect("writes the input");
    let staging = Staging {
        inputs: vec![Staged {
            path: "in.txt".into(),
            source,
            directory: false,
            copy: false,
            executable: false,
            ..Default::default()
        }],
        ..Staging::default()
    };
    let spec = spec("debian:stable-slim", "tr a-z A-Z < in.txt > out.txt");
    let placement = placement();
    let job = Job {
        name: "run-P-1-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let finished = (backend.run(job, &CancellationToken::new()).await).expect("the job ends");
    assert_eq!(finished.exit_code, Some(0));
    let output = std::fs::read_to_string(finished.local_path("out.txt")).expect("an output");
    assert_eq!(output, "HELLO\n");
    // The Job is created, its pod looked at, and the Job deleted once it ended.
    let calls = state.lock().expect("the state").calls.clone();
    assert_eq!(
        calls.first().map(String::as_str),
        Some(&*format!("POST {JOBS}"))
    );
    let deleted = format!("DELETE {JOBS}/iunctus-run-p-1-1");
    assert!(calls.contains(&deleted), "{calls:?}");
    assert!(
        calls
            .last()
            .is_some_and(|call| call == &format!("GET {PODS}")),
        "{calls:?}"
    );
}

#[tokio::test]
async fn uses_image_entrypoint() {
    let root = tempfile::tempdir().expect("a folder");
    let (backend, _) = backend(root.path()).await;
    let spec = spec("with-entrypoint", "echo $FROM_ENTRYPOINT > out.txt");
    let (staging, placement) = (Staging::default(), placement());
    let job = Job {
        name: "run-P-5-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let finished = (backend.run(job, &CancellationToken::new()).await).expect("the job ends");
    assert_eq!(finished.exit_code, Some(0));
    let output = std::fs::read_to_string(finished.local_path("out.txt")).expect("an output");
    assert_eq!(output, "yes\n");
}

#[tokio::test]
async fn binds_selected_platform() {
    let root = tempfile::tempdir().unwrap();
    let (backend, state) = backend(root.path()).await;
    let config = std::fs::read_to_string(root.path().join("kubeconfig")).unwrap();
    let address = config
        .lines()
        .find_map(|line| line.trim().strip_prefix("server: http://"))
        .unwrap();
    let reference = format!("localhost:{}/tool", address.rsplit(':').next().unwrap());
    let mut spec = spec(&format!("{reference}:1"), "echo $FROM_ENTRYPOINT > out.txt");
    spec.image.as_mut().unwrap().platform = Some("linux/arm64".into());
    let (staging, placement) = (Staging::default(), placement());
    let job = Job {
        name: "platform",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let finished = backend.run(job, &CancellationToken::new()).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(finished.local_path("out.txt")).unwrap(),
        "arm\n"
    );
    let state = state.lock().unwrap();
    let pod = &state.submitted[0]["spec"]["template"]["spec"];
    assert_eq!(
        pod["containers"][0]["image"],
        format!("{reference}@sha256:aa")
    );
    assert_eq!(pod["nodeSelector"]["kubernetes.io/arch"], "arm64");
    assert_eq!(pod["nodeSelector"]["kubernetes.io/os"], "linux");
}

#[tokio::test]
async fn inherits_pod_platform() {
    let root = tempfile::tempdir().unwrap();
    let (backend, state) = backend(root.path()).await;
    let config = std::fs::read_to_string(root.path().join("kubeconfig")).unwrap();
    let address = config
        .lines()
        .find_map(|line| line.trim().strip_prefix("server: http://"))
        .unwrap();
    let registry = format!("localhost:{}", address.rsplit(':').next().unwrap());
    let mut spec = spec(&format!("{registry}/tool:1"), "true");
    let main = Image {
        reference: format!("{registry}/tool:1"),
        platform: Some("linux/arm64".into()),
    };
    spec.containers = vec![
        ContainerSpec {
            name: "main".into(),
            image: Some(main),
            replaces_entrypoint: false,
            arguments: vec![
                "/bin/bash".into(),
                "-c".into(),
                "cat secondary.txt > out.txt".into(),
            ],
            workdir: None,
            environment: BTreeMap::new(),
            after: vec!["secondary".into()],
            attempts: 1,
        },
        ContainerSpec {
            name: "secondary".into(),
            image: Some(Image {
                reference: format!("{registry}/secondary:1"),
                platform: None,
            }),
            replaces_entrypoint: false,
            arguments: Vec::new(),
            workdir: None,
            environment: BTreeMap::new(),
            after: Vec::new(),
            attempts: 1,
        },
    ];
    let (staging, placement) = (Staging::default(), placement());
    let job = Job {
        name: "mixed-platform",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let finished = backend.run(job, &CancellationToken::new()).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(finished.local_path("out.txt")).unwrap(),
        "arm-arm-default\n"
    );
    {
        let state = state.lock().unwrap();
        let pod = &state.submitted[0]["spec"]["template"]["spec"];
        assert_eq!(
            pod["containers"][1]["image"],
            format!("{registry}/secondary@sha256:dd")
        );
        assert_eq!(pod["nodeSelector"]["kubernetes.io/arch"], "arm64");
        assert!(
            !state
                .calls
                .iter()
                .any(|call| call.contains("sha256:ee") || call.contains("sha256:gg"))
        );
    }
    spec.containers[1].image.as_mut().unwrap().platform = Some("linux/amd64".into());
    let job = Job {
        name: "conflicting-platform",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    assert!(matches!(
        backend.run(job, &CancellationToken::new()).await,
        Err(ExecutorError::Unsupported(_))
    ));
    assert_eq!(state.lock().unwrap().submitted.len(), 1);
}

#[tokio::test]
async fn rejects_gpu_groups() {
    let root = tempfile::tempdir().unwrap();
    let (backend, state) = backend(root.path()).await;
    let mut spec = spec("debian:stable-slim", "true");
    spec.containers = ["debian:stable-slim", "with-entrypoint"]
        .into_iter()
        .enumerate()
        .map(|(index, image)| ContainerSpec {
            replaces_entrypoint: true,
            name: format!("container-{index}"),
            image: Some(Image {
                reference: image.into(),
                platform: None,
            }),
            arguments: vec!["true".into()],
            workdir: None,
            environment: BTreeMap::new(),
            after: Vec::new(),
            attempts: 1,
        })
        .collect();
    let staging = Staging::default();
    let mut placement = placement();
    placement.accelerators.push(task_model::Accelerator {
        count: 1,
        kind: None,
    });
    let job = Job {
        name: "gpu-groups",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    assert!(matches!(
        backend.run(job, &CancellationToken::new()).await,
        Err(ExecutorError::Unsupported(_))
    ));
    assert!(state.lock().unwrap().submitted.is_empty());
    assert!(!backend.scratch.join("gpu-groups").exists());
}

#[tokio::test]
async fn reports_tool_failures() {
    let root = tempfile::tempdir().expect("a folder");
    let (backend, _) = backend(root.path()).await;
    let spec = spec("debian:stable-slim", "echo broken >&2; exit 3");
    let (staging, placement) = (Staging::default(), placement());
    let job = Job {
        name: "run-P-2-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let finished = (backend.run(job, &CancellationToken::new()).await).expect("the job ends");
    assert_eq!(finished.exit_code, Some(3));
    let stderr = std::fs::read_to_string(finished.stderr()).expect("the standard error");
    assert_eq!(stderr, "broken\n");
}

#[tokio::test]
async fn reports_wrapper_failures() {
    let root = tempfile::tempdir().expect("a folder");
    let (backend, _) = backend(root.path()).await;
    let spec = spec("without-shell", "true");
    let (staging, placement) = (Staging::default(), placement());
    let job = Job {
        name: "run-P-3-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let error = (backend.run(job, &CancellationToken::new()).await).expect_err("a failure");
    assert!(error.retryable(), "{error}");
    assert!(error.to_string().contains("\"sh\": not found"), "{error}");
}

#[tokio::test]
async fn cancelled_jobs_stop() {
    let root = tempfile::tempdir().expect("a folder");
    let (backend, state) = backend(root.path()).await;
    let spec = spec("debian:stable-slim", "sleep 30");
    let (staging, placement) = (Staging::default(), placement());
    let job = Job {
        name: "run-P-4-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let cancel = CancellationToken::new();
    let stopper = {
        let cancel = cancel.clone();
        async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            cancel.cancel();
        }
    };
    let (result, ()) = tokio::join!(backend.run(job, &cancel), stopper);
    assert!(
        matches!(result, Err(ExecutorError::Cancelled)),
        "{result:?}"
    );
    let state = state.lock().expect("the state");
    assert!(state.jobs["iunctus-run-p-4-1"].deleted);
    assert!(!root.path().join("scratch/run-P-4-1").exists());
}

#[tokio::test]
async fn retains_ambiguous_submissions() {
    let root = tempfile::tempdir().unwrap();
    let (backend, state) = backend(root.path()).await;
    std::fs::write(root.path().join("server/lose-submit"), "").unwrap();
    let spec = spec("debian:stable-slim", "true");
    let (staging, placement) = (Staging::default(), placement());
    let job = Job {
        name: "ambiguous",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let error = backend
        .run(job, &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(!error.retryable());
    let journal = backend.scratch.join("ambiguous/remote.json");
    let mut record: Value = serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
    assert!(record["uid"].is_null());
    assert_eq!(state.lock().unwrap().submitted.len(), 1);
    record["owner"] = json!(0);
    std::fs::write(&journal, record.to_string()).unwrap();
    let blocked = Job {
        name: "blocked",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let error = backend
        .run(blocked, &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("UID was not recorded"));
    assert!(journal.is_file());
    assert_eq!(state.lock().unwrap().submitted.len(), 1);
}

#[tokio::test]
async fn recovers_dropped_attempts() {
    let root = tempfile::tempdir().unwrap();
    let (backend, state) = backend(root.path()).await;
    let old_spec = spec("debian:stable-slim", "sleep 30");
    let (staging, placement) = (Staging::default(), placement());
    let old = Job {
        name: "orphan",
        spec: &old_spec,
        staging: &staging,
        placement: &placement,
    };
    assert!(
        tokio::time::timeout(
            Duration::from_millis(600),
            backend.run(old, &CancellationToken::new())
        )
        .await
        .is_err()
    );
    let record: Value =
        serde_json::from_slice(&std::fs::read(backend.scratch.join("orphan/remote.json")).unwrap())
            .unwrap();
    assert_eq!(record["owner"], json!(std::process::id()));

    let next_spec = spec("debian:stable-slim", "echo next > out.txt");
    let next = Job {
        name: "next",
        spec: &next_spec,
        staging: &staging,
        placement: &placement,
    };
    backend.run(next, &CancellationToken::new()).await.unwrap();
    let state = state.lock().unwrap();
    assert!(state.jobs["iunctus-orphan"].deleted);
    assert_eq!(state.submitted.len(), 2);
}

#[tokio::test]
async fn preserves_active_siblings() {
    let root = tempfile::tempdir().unwrap();
    let (backend, state) = backend(root.path()).await;
    let active_spec = spec("debian:stable-slim", "sleep 1; echo active > out.txt");
    let next_spec = spec("debian:stable-slim", "echo next > out.txt");
    let (staging, placement, cancel) = (Staging::default(), placement(), CancellationToken::new());
    let active = Job {
        name: "active",
        spec: &active_spec,
        staging: &staging,
        placement: &placement,
    };
    let next = async {
        tokio::time::sleep(Duration::from_millis(400)).await;
        let next = Job {
            name: "sibling",
            spec: &next_spec,
            staging: &staging,
            placement: &placement,
        };
        let result = backend.run(next, &cancel).await;
        assert!(!state.lock().unwrap().jobs["iunctus-active"].deleted);
        result
    };
    let (active, next) = tokio::join!(backend.run(active, &cancel), next);
    active.unwrap();
    next.unwrap();
    let state = state.lock().unwrap();
    assert_eq!(state.submitted.len(), 2);
}

#[tokio::test]
async fn cancellation_prevents_submission() {
    let root = tempfile::tempdir().unwrap();
    let (backend, state) = backend(root.path()).await;
    let spec = spec("debian:stable-slim", "true");
    let (staging, placement) = (Staging::default(), placement());
    let job = Job {
        name: "cancelled-before-admission",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        backend.run(job, &cancel).await,
        Err(ExecutorError::Cancelled)
    ));
    assert!(state.lock().unwrap().submitted.is_empty());
}

#[tokio::test]
async fn recovery_waits_removal() {
    let root = tempfile::tempdir().unwrap();
    let (backend, state) = backend(root.path()).await;
    let spec = spec("debian:stable-slim", "sleep 30");
    let (staging, placement) = (Staging::default(), placement());
    let job = Job {
        name: "recover",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    assert!(
        tokio::time::timeout(
            Duration::from_millis(600),
            backend.run(job, &CancellationToken::new())
        )
        .await
        .is_err()
    );
    let journal = backend.scratch.join("recover/remote.json");
    let mut record: Value = serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
    let uid = record["uid"].clone();
    record["owner"] = json!(0);
    std::fs::write(&journal, record.to_string()).unwrap();
    state.lock().unwrap().submitted[0]["metadata"]["uid"] = json!("replacement");
    let recovered = KubernetesBackend::new(backend.cluster.clone(), backend.scratch.clone());
    let error = recovered.reconcile().await.unwrap_err();
    assert!(error.to_string().contains("identity changed"));
    assert!(journal.is_file());
    assert!(!state.lock().unwrap().jobs["iunctus-recover"].deleted);
    state.lock().unwrap().submitted[0]["metadata"]["uid"] = uid;
    std::fs::write(root.path().join("server/delay-delete"), "").unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(200), recovered.reconcile())
            .await
            .is_err()
    );
    assert!(journal.is_file());
    assert!(!state.lock().unwrap().jobs["iunctus-recover"].deleted);
    std::fs::remove_file(root.path().join("server/delay-delete")).unwrap();
    recovered.reconcile().await.unwrap();
    assert!(!journal.exists());
    assert!(state.lock().unwrap().jobs["iunctus-recover"].deleted);
}

#[tokio::test]
async fn failure_retains_scratch() {
    let root = tempfile::tempdir().unwrap();
    let (backend, state) = backend(root.path()).await;
    let delay = root.path().join("server/delay-delete");
    std::fs::write(&delay, "").unwrap();
    let mut spec = spec("debian:stable-slim", "true");
    spec.containers = ["without-shell", "debian:stable-slim"]
        .into_iter()
        .enumerate()
        .map(|(index, image)| ContainerSpec {
            name: format!("container-{index}"),
            image: Some(Image {
                reference: image.into(),
                platform: None,
            }),
            replaces_entrypoint: true,
            arguments: vec!["/bin/bash".into(), "-c".into(), "sleep 30".into()],
            workdir: None,
            environment: BTreeMap::new(),
            after: Vec::new(),
            attempts: 1,
        })
        .collect();
    let (staging, placement) = (Staging::default(), placement());
    let job = Job {
        name: "siblings",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let cancel = CancellationToken::new();
    let run = backend.run(job, &cancel);
    let confirm = async {
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert!(backend.scratch.join("siblings/remote.json").is_file());
        assert!(backend.scratch.join("siblings/control").is_dir());
        assert!(!state.lock().unwrap().jobs["iunctus-siblings"].deleted);
        std::fs::remove_file(&delay).unwrap();
    };
    let (result, ()) = tokio::join!(run, confirm);
    assert!(result.is_err());
    assert!(!backend.scratch.join("siblings").exists());
    assert!(state.lock().unwrap().jobs["iunctus-siblings"].deleted);
}

#[tokio::test]
async fn rejects_missing_kubeconfig() {
    let root = tempfile::tempdir().expect("a folder");
    let missing = root.path().join("missing.yaml");
    let error = Cluster::connect(&missing, None)
        .await
        .expect_err("an error");
    assert!(error.contains("missing.yaml"), "{error}");
}

#[tokio::test]
async fn containers_share_pod() {
    let root = tempfile::tempdir().expect("a folder");
    let (backend, state) = backend(root.path()).await;
    let container = |name: &str, image: &str, script: &str, after: &[&str]| ContainerSpec {
        replaces_entrypoint: false,
        name: name.into(),
        image: Some(Image {
            reference: image.into(),
            platform: None,
        }),
        arguments: vec!["/bin/bash".into(), "-c".into(), script.into()],
        workdir: None,
        environment: BTreeMap::new(),
        after: after.iter().map(|name| (*name).to_owned()).collect(),
        attempts: 1,
    };
    let mut spec = spec("debian:stable-slim", "cat first.txt > out.txt");
    spec.containers = vec![
        container(
            "main",
            "debian:stable-slim",
            "cat first.txt > out.txt",
            &["first"],
        ),
        container(
            "first",
            "with-entrypoint",
            "echo $FROM_ENTRYPOINT > first.txt",
            &[],
        ),
    ];
    let (staging, placement) = (Staging::default(), placement());
    let job = Job {
        name: "run-S-1-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let finished = (backend.run(job, &CancellationToken::new()).await).expect("the job ends");
    assert_eq!(finished.exit_code, Some(0));
    let output = std::fs::read_to_string(finished.local_path("out.txt")).expect("an output");
    assert_eq!(output, "yes\n");
    // One Job with a container for each image.
    let calls = state.lock().expect("the state").calls.clone();
    let created = calls.iter().filter(|call| call.starts_with("POST")).count();
    assert_eq!(created, 1, "{calls:?}");
    let control = finished.root.join(executor::CONTROL);
    let ended = executor::containers::ended(&spec, &control);
    let codes: Vec<Option<i32>> = ended.iter().map(|ended| ended.exit_code).collect();
    assert_eq!(codes, [Some(0), Some(0)]);
}
