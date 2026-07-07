//! The Kubernetes backend against a simulated `kubectl` on this machine: `create` runs the
//! command of the Job's container in the background, without a container, which works because
//! the pod sees the files at the same paths; `get pods` tells how each one stands, and
//! `delete job` stops it. The simulated `kubectl` fails every call that does not name the
//! kubeconfig of the test, so the tests also show that the backend never uses another one.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use executor::{Backend, ExecutorError, Job, Staged, Staging};
use kubernetes_executor::{Kubectl, KubernetesBackend};
use task_model::{FORMAT, Image, OutputRule, Placement, TaskSpec};
use tokio_util::sync::CancellationToken;

const KUBECTL: &str = r#"#!/usr/bin/env python3
import json, os, signal, subprocess, sys
from pathlib import Path

state = Path(__file__).parent / "state"
state.mkdir(exist_ok=True)
arguments = sys.argv[1:]
expected = Path(__file__).parent / "kubeconfig"
if "--kubeconfig" not in arguments or os.environ.get("KUBECONFIG") != str(expected):
    sys.exit("error: no kubeconfig of the test")
place = arguments.index("--kubeconfig")
if arguments[place + 1] != str(expected):
    sys.exit("error: another kubeconfig")
del arguments[place:place + 2]
if "--namespace" in arguments:
    place = arguments.index("--namespace")
    del arguments[place:place + 2]
with open(state / "calls", "a") as calls:
    calls.write(" ".join(arguments[:2]) + "\n")

if arguments[:1] == ["create"]:
    job = json.load(sys.stdin)
    name = job["metadata"]["name"]
    container = job["spec"]["template"]["spec"]["containers"][0]
    words = " ".join("'" + word.replace("'", "'\\''") + "'" for word in container["command"])
    script = state / f"{name}.sh"
    script.write_text(f"cd '{container['workingDir']}' && {words}\necho $? > '{state / name}.exit'\n")
    (state / f"{name}.json").write_text(json.dumps(job))
    log = open(state / f"{name}.log", "w")
    process = subprocess.Popen(["bash", str(script)], stdout=log, stderr=log, start_new_session=True)
    (state / f"{name}.pid").write_text(str(process.pid))
    print(f"job.batch/{name} created")
elif arguments[:2] == ["get", "pods"]:
    items = []
    for record in sorted(state.glob("*.json")):
        name = record.stem
        if (state / f"{name}.deleted").exists():
            continue
        exit = state / f"{name}.exit"
        if exit.exists():
            code = int(exit.read_text().strip())
            reason = "Completed" if code == 0 else "Error"
            status = {"phase": "Succeeded" if code == 0 else "Failed", "containerStatuses": [
                {"state": {"terminated": {"exitCode": code, "reason": reason}}}]}
        else:
            status = {"phase": "Running", "containerStatuses": [{"state": {"running": {}}}]}
        items.append({"metadata": {"labels": {"job-name": name}}, "status": status})
    print(json.dumps({"items": items}))
elif arguments[:2] == ["delete", "job"]:
    name = arguments[2]
    (state / f"{name}.deleted").touch()
    pid = state / f"{name}.pid"
    if pid.exists():
        try:
            os.killpg(int(pid.read_text()), signal.SIGTERM)
        except ProcessLookupError:
            pass
    print(f"job.batch \"{name}\" deleted")
else:
    sys.exit(f"error: unknown call {arguments}")
"#;

/// The simulated `kubectl` and its kubeconfig in a folder of `root`.
fn cluster(root: &Path) -> Kubectl {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).expect("makes the folder");
    let kubectl = bin.join("kubectl");
    std::fs::write(&kubectl, KUBECTL).expect("writes kubectl");
    std::fs::set_permissions(&kubectl, std::fs::Permissions::from_mode(0o755))
        .expect("makes it executable");
    let kubeconfig = bin.join("kubeconfig");
    std::fs::write(&kubeconfig, "apiVersion: v1\nkind: Config\n").expect("writes the kubeconfig");
    Kubectl {
        program: kubectl,
        kubeconfig,
        namespace: Some("iunctus-test".into()),
    }
}

fn backend(root: &Path) -> (KubernetesBackend, PathBuf) {
    let kubectl = cluster(root);
    let state = root.join("bin/state");
    let mut backend = KubernetesBackend::new(kubectl, root.join("scratch"));
    backend.poll = Duration::from_millis(100);
    (backend, state)
}

fn spec(script: &str) -> TaskSpec {
    TaskSpec {
        format: FORMAT,
        namespace: String::new(),
        image: Some(Image {
            reference: "debian:stable-slim".into(),
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
async fn jobs_run_tasks_and_give_their_outputs() {
    let root = tempfile::tempdir().expect("a folder");
    let (backend, state) = backend(root.path());
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
        }],
        ..Staging::default()
    };
    let spec = spec("tr a-z A-Z < in.txt > out.txt");
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
    let manifest = std::fs::read_to_string(state.join("iunctus-run-p-1-1.json")).expect("a Job");
    assert!(manifest.contains("\"memory\": \"268435456\""), "{manifest}");
    // The Job is deleted once it ended.
    let calls = std::fs::read_to_string(state.join("calls")).expect("the calls");
    assert!(calls.ends_with("delete job\n"), "{calls}");
}

#[tokio::test]
async fn failed_tools_end_their_tasks() {
    let root = tempfile::tempdir().expect("a folder");
    let (backend, _) = backend(root.path());
    let spec = spec("echo broken >&2; exit 3");
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
async fn cancelled_jobs_stop() {
    let root = tempfile::tempdir().expect("a folder");
    let (backend, state) = backend(root.path());
    let spec = spec("sleep 30");
    let (staging, placement) = (Staging::default(), placement());
    let job = Job {
        name: "run-P-3-1",
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
    assert!(state.join("iunctus-run-p-3-1.deleted").exists());
    assert!(!root.path().join("scratch/run-P-3-1").exists());
}
