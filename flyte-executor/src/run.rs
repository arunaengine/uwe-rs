//! The Flyte run of an attempt: its name, the `CreateRun` message of its raw-container task,
//! and its state as `GetRunDetails` tells it.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use task_model::Placement;

use crate::client::field;

pub const CREATE: &str = "flyteidl2.workflow.RunService/CreateRun";
pub const DETAILS: &str = "flyteidl2.workflow.RunService/GetRunDetails";
pub const ABORT: &str = "flyteidl2.workflow.RunService/AbortRun";
pub const LOGS: &str = "flyteidl2.workflow.RunLogsService/TailLogs";

/// The input variable of the helper, which Flyte downloads before the container starts.
pub const HELPER: &str = "uwe";

/// The action of a run that runs its task.
pub const ROOT: &str = "a0";

/// The project that holds the runs.
#[derive(Clone, Debug)]
pub struct Project {
    pub org: String,
    pub project: String,
    pub domain: String,
}

impl Project {
    /// The `RunIdentifier` of the run `name`.
    pub fn run(&self, name: &str) -> Value {
        json!({"org": self.org, "project": self.project, "domain": self.domain, "name": name})
    }
}

/// What the run of an attempt runs.
pub struct Shape<'a> {
    pub name: &'a str,
    pub attempt: &'a str,
    pub image: &'a str,
    /// The arguments of the container, after the entrypoint of its image.
    pub args: Vec<String>,
    pub placement: &'a Placement,
    /// How long the task may run, with the time for staging.
    pub seconds: Option<u64>,
    pub queue: Option<&'a str>,
    /// The object address of the helper, which Flyte downloads to `inputs`.
    pub helper: Option<&'a str>,
    /// Where the container sees the inputs that Flyte downloads.
    pub inputs: &'a str,
}

/// A run name of at most 30 letters, as Flyte allows, unique for the attempt and the moment.
pub fn run_name(attempt: &str, moment: u128) -> String {
    let digest = Sha256::digest(format!("{attempt}\n{moment}").as_bytes());
    let hex: String = digest
        .iter()
        .take(12)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("uwe-{hex}")
}

/// The `CreateRunRequest` of the run of `shape` in `project`.
pub fn body(shape: &Shape<'_>, project: &Project) -> Value {
    let name: String = shape
        .attempt
        .chars()
        .map(
            |letter| match letter.is_ascii_alphanumeric() || "_.-".contains(letter) {
                true => letter,
                false => '_',
            },
        )
        .collect();
    let task = format!("uwe.{name}");
    let task: String = task.chars().take(255).collect();
    let short: String = task.chars().take(63).collect();
    let placement = shape.placement;
    let mut resources = vec![json!({"name": "CPU", "value": placement.cpus.max(1).to_string()})];
    let mebibytes = |bytes: u64| format!("{}Mi", bytes.div_ceil(1 << 20));
    if let Some(memory) = placement.memory_bytes {
        resources.push(json!({"name": "MEMORY", "value": mebibytes(memory)}));
    }
    if let Some(disk) = placement.disk_bytes {
        resources.push(json!({"name": "EPHEMERAL_STORAGE", "value": mebibytes(disk)}));
    }
    let gpus: u32 = placement.accelerators.iter().map(|gpu| gpu.count).sum();
    if gpus > 0 {
        resources.push(json!({"name": "GPU", "value": gpus.to_string()}));
    }
    let mut container = json!({
        "image": shape.image,
        "args": shape.args,
        "resources": {"requests": resources, "limits": resources},
    });
    let mut variables = Vec::new();
    let mut inputs = Vec::new();
    if let Some(helper) = shape.helper {
        let blob = json!({"format": "", "dimensionality": "SINGLE"});
        variables.push(json!({"key": HELPER, "value": {"type": {"blob": blob}}}));
        let literal = json!({"scalar": {"blob": {"metadata": {"type": blob}, "uri": helper}}});
        inputs.push(json!({"name": HELPER, "value": literal}));
        container["dataConfig"] = json!({
            "enabled": true,
            "inputPath": shape.inputs,
            "outputPath": format!("{}.out", shape.inputs),
            "format": "JSON",
            "ioStrategy": {"downloadMode": "DOWNLOAD_EAGER", "uploadMode": "DO_NOT_UPLOAD"},
        });
    }
    let mut metadata = json!({
        "runtime": {"type": "OTHER", "flavor": "uwe", "version": env!("CARGO_PKG_VERSION")},
        "retries": {"retries": 0},
        "discoverable": false,
    });
    if let Some(seconds) = shape.seconds {
        metadata["timeout"] = json!(format!("{seconds}s"));
    }
    if placement.preemptible {
        metadata["interruptible"] = json!(true);
    }
    let mut template = json!({
        "id": {
            "resourceType": "TASK",
            "org": project.org,
            "project": project.project,
            "domain": project.domain,
            "name": task,
            "version": env!("CARGO_PKG_VERSION"),
        },
        "type": "raw-container",
        "metadata": metadata,
        "interface": {"inputs": {"variables": variables}, "outputs": {"variables": []}},
        "container": container,
    });
    let kind = (placement.accelerators.iter()).find_map(|gpu| gpu.kind.as_deref());
    if let Some(kind) = kind {
        template["extendedResources"] = json!({"gpuAccelerator": {"device": kind}});
    }
    let mut spec = Map::new();
    if let Some(queue) = shape.queue {
        spec.insert("cluster".into(), json!(queue));
    }
    if !placement.labels.is_empty() {
        spec.insert("labels".into(), json!({"values": placement.labels}));
    }
    json!({
        "runId": project.run(shape.name),
        "taskSpec": {"taskTemplate": template, "shortName": short},
        "inputs": {"literals": inputs},
        "runSpec": spec,
    })
}

/// The phase of an action, from its name or its number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Waiting,
    Running,
    Succeeded,
    Failed,
    Aborted,
    TimedOut,
}

/// What `GetRunDetails` says about the root action of a run.
#[derive(Debug, PartialEq, Eq)]
pub struct Seen {
    pub phase: Phase,
    /// Why it failed or was aborted, as Flyte says it.
    pub message: String,
    /// The number of its latest attempt, for its log.
    pub attempt: u64,
}

pub fn seen(answer: &Value) -> Seen {
    let action = field(&answer["details"], "action");
    let status = field(action, "status");
    let phase = match &field(status, "phase") {
        Value::String(name) => name.trim_start_matches("ACTION_PHASE_").to_owned(),
        Value::Number(number) => match number.as_u64() {
            Some(4) => "RUNNING",
            Some(5) => "SUCCEEDED",
            Some(6) => "FAILED",
            Some(7) => "ABORTED",
            Some(8) => "TIMED_OUT",
            _ => "QUEUED",
        }
        .to_owned(),
        _ => "QUEUED".to_owned(),
    };
    let phase = match phase.as_str() {
        "RUNNING" => Phase::Running,
        "SUCCEEDED" => Phase::Succeeded,
        "FAILED" => Phase::Failed,
        "ABORTED" => Phase::Aborted,
        "TIMED_OUT" => Phase::TimedOut,
        _ => Phase::Waiting,
    };
    let message = (field(field(action, "errorInfo"), "message").as_str())
        .or(field(field(action, "abortInfo"), "reason").as_str())
        .unwrap_or_default()
        .to_owned();
    let attempt = (field(status, "attempts").as_u64())
        .or_else(|| field(status, "attempts").as_str()?.parse().ok())
        .unwrap_or(1)
        .max(1);
    Seen {
        phase,
        message,
        attempt,
    }
}

/// The text of the log lines of `TailLogs` messages.
pub fn log_text(messages: &[Value]) -> String {
    let mut text = String::new();
    for message in messages {
        for logs in message["logs"].as_array().into_iter().flatten() {
            for line in logs["lines"].as_array().into_iter().flatten() {
                let line = line["message"].as_str().unwrap_or_default();
                text.push_str(line);
                if !line.ends_with('\n') {
                    text.push('\n');
                }
            }
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_model::Accelerator;

    #[test]
    fn builds_raw_containers() {
        let placement = Placement {
            cpus: 2,
            memory_bytes: Some(3 << 30),
            disk_bytes: Some(10 << 30),
            accelerators: vec![Accelerator {
                count: 1,
                kind: Some("nvidia-tesla-t4".into()),
            }],
            labels: [("team".to_owned(), "x".to_owned())].into(),
            ..Placement::default()
        };
        let project = Project {
            org: "lab".into(),
            project: "flows".into(),
            domain: "development".into(),
        };
        let shape = Shape {
            name: "uwe-0123",
            attempt: "run/ALIGN 1",
            image: "ubuntu:24.04",
            args: vec!["sh".into(), "-c".into(), "true".into()],
            placement: &placement,
            seconds: Some(3900),
            queue: Some("gpu"),
            helper: Some("s3://bucket/cas/ab"),
            inputs: "/tmp/uwe/a.in",
        };
        let body = body(&shape, &project);
        assert_eq!(body["runId"]["name"], "uwe-0123");
        let template = &body["taskSpec"]["taskTemplate"];
        assert_eq!(template["type"], "raw-container");
        assert_eq!(template["id"]["name"], "uwe.run_ALIGN_1");
        assert_eq!(template["metadata"]["timeout"], "3900s");
        assert_eq!(template["metadata"]["retries"]["retries"], 0);
        let container = &template["container"];
        assert_eq!(container["args"][2], "true");
        assert!(
            container.get("command").is_none(),
            "the entrypoint of the image stays"
        );
        let requests = &container["resources"]["requests"];
        assert_eq!(requests, &container["resources"]["limits"]);
        let names: Vec<_> = (requests.as_array().into_iter().flatten())
            .map(|entry| {
                format!(
                    "{}={}",
                    entry["name"].as_str().unwrap_or(""),
                    entry["value"]
                )
            })
            .collect();
        assert_eq!(
            names,
            [
                "CPU=\"2\"",
                "MEMORY=\"3072Mi\"",
                "EPHEMERAL_STORAGE=\"10240Mi\"",
                "GPU=\"1\""
            ]
        );
        assert_eq!(container["dataConfig"]["inputPath"], "/tmp/uwe/a.in");
        assert_eq!(
            template["extendedResources"]["gpuAccelerator"]["device"],
            "nvidia-tesla-t4"
        );
        let literal = &body["inputs"]["literals"][0];
        assert_eq!(literal["name"], HELPER);
        assert_eq!(
            literal["value"]["scalar"]["blob"]["uri"],
            "s3://bucket/cas/ab"
        );
        assert_eq!(body["runSpec"]["cluster"], "gpu");
        assert_eq!(body["runSpec"]["labels"]["values"]["team"], "x");
        let name = run_name("run-ALIGN-1-1", 7);
        assert!(name.len() <= 30 && name.starts_with("uwe-"), "{name}");
        assert_ne!(name, run_name("run-ALIGN-1-1", 8));
    }

    #[test]
    fn reads_run_details() {
        let answer = json!({"details": {"action": {
            "status": {"phase": "ACTION_PHASE_FAILED", "attempts": 2},
            "errorInfo": {"message": "exit 1", "kind": "KIND_USER"}}}});
        let seen = seen(&answer);
        assert_eq!((seen.phase, seen.attempt), (Phase::Failed, 2));
        assert_eq!(seen.message, "exit 1");
        let running = json!({"details": {"action": {"status": {"phase": 4}}}});
        assert_eq!(super::seen(&running).phase, Phase::Running);
        assert_eq!(super::seen(&json!({})).phase, Phase::Waiting);
        let logs = json!([{"logs": [{"lines": [{"message": "a"}, {"message": "b\n"}]}]}]);
        assert_eq!(log_text(logs.as_array().expect("a list")), "a\nb\n");
    }
}
