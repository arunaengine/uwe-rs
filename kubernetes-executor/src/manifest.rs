//! The Kubernetes Job of an attempt: one pod with one container, which runs the wrapper of the
//! task as the user, with the folders of the task as `hostPath` volumes at the same paths.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Value, json};

use executor::ContainerRun;

/// The label that marks the Jobs of one backend, so that one request lists all their pods.
pub const LABEL: &str = "iunctus.dev/attempts";

/// How long Kubernetes keeps a Job that ended, in case the backend could not delete it.
const KEEP_SECONDS: u64 = 600;

/// The name of the Job of an attempt: a DNS label of at most 63 lowercase letters, digits and
/// dashes. A long name keeps its start and gets a digest of the whole name, so that names
/// stay unique.
pub fn job_name(name: &str) -> String {
    let mut clean: String = format!("iunctus-{name}")
        .chars()
        .map(|character| match character.to_ascii_lowercase() {
            letter @ ('a'..='z' | '0'..='9') => letter,
            _ => '-',
        })
        .collect();
    if clean.len() > 63 {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(name.as_bytes());
        let short: String = digest[..4]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        clean.truncate(54);
        clean = format!("{}-{short}", clean.trim_end_matches('-'));
    }
    clean.trim_end_matches('-').to_owned()
}

/// What a Job asks for besides its container.
#[derive(Clone, Debug, Default)]
pub struct Limits {
    /// The GPUs of the task.
    pub gpus: u32,
    /// How long the task may run.
    pub seconds: Option<u64>,
}

/// The Job that runs the wrapper of each of `runs` in its image, labelled with `label`: one
/// container for a task, or one for each image of a task of several containers, which share
/// the folders of the task. The first container asks for the CPUs and memory of the task, and
/// each may use all of its memory.
pub fn job(name: &str, label: &str, runs: &[ContainerRun<'_>], limits: &Limits) -> Value {
    let run = &runs[0];
    let mut mounts = Vec::new();
    let mut volumes = Vec::new();
    for (index, bind) in run.binds().iter().enumerate() {
        let volume = format!("folder-{index}");
        mounts.push(json!({
            "name": volume,
            "mountPath": bind.target,
            "readOnly": bind.read_only,
        }));
        let source = bind.source.to_string_lossy();
        volumes.push(json!({"name": volume, "hostPath": {"path": source}}));
    }
    let mut requests = json!({"cpu": run.cpus.max(1).to_string()});
    let mut limits_of_container = json!({});
    if let Some(memory) = run.memory {
        requests["memory"] = json!(memory.to_string());
        limits_of_container["memory"] = json!(memory.to_string());
    }
    if limits.gpus > 0 {
        limits_of_container["nvidia.com/gpu"] = json!(limits.gpus.to_string());
    }
    let labels = json!({"app.kubernetes.io/managed-by": "iunctus", LABEL: label});
    let containers: Vec<Value> = (runs.iter().enumerate())
        .map(|(index, run)| {
            let (name, requests) = match (runs.len(), index) {
                (1, _) => ("task".to_owned(), requests.clone()),
                (_, 0) => ("task-0".to_owned(), requests.clone()),
                (_, index) => (format!("task-{index}"), json!({"cpu": "0", "memory": "0"})),
            };
            json!({
                "name": name,
                "image": run.image.reference,
                "command": ["sh", run.script()],
                "workingDir": run.inside(),
                "resources": {"requests": requests, "limits": limits_of_container},
                "volumeMounts": mounts,
            })
        })
        .collect();
    let mut spec = json!({
        "backoffLimit": 0,
        "ttlSecondsAfterFinished": KEEP_SECONDS,
        "template": {
            "metadata": {"labels": labels},
            "spec": {
                "restartPolicy": "Never",
                "securityContext": {"runAsUser": run.owner.0, "runAsGroup": run.owner.1},
                "containers": containers,
                "volumes": volumes,
            },
        },
    });
    // The time counts on the pod, from its start, so that waiting for a node does not count.
    if let Some(seconds) = limits.seconds {
        spec["template"]["spec"]["activeDeadlineSeconds"] = json!(seconds);
    }
    json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {"name": name, "labels": labels},
        "spec": spec,
    })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use executor::process::Mount;
    use task_model::Image;

    use super::*;

    #[test]
    fn names_are_dns_labels() {
        assert_eq!(
            job_name("1a0f-49038-UPPER-1-1"),
            "iunctus-1a0f-49038-upper-1-1"
        );
        let long = job_name(&format!("run-{}-1", "NFCORE_DEMO:DEMO:FASTQC".repeat(4)));
        assert!(long.len() <= 63, "{long}");
        assert!(
            long.starts_with("iunctus-run-nfcore-demo-demo-fastqc"),
            "{long}"
        );
        assert_ne!(
            long,
            job_name(&format!("run-{}-2", "NFCORE_DEMO:DEMO:FASTQC".repeat(4)))
        );
    }

    #[test]
    fn jobs_run_the_wrapper_as_the_user_with_the_folders_of_the_task() {
        let image = Image {
            reference: "debian@sha256:ab".into(),
            platform: None,
        };
        let mounts = [Mount {
            source: PathBuf::from("/cache/store/cas"),
            target: "/cache/store/cas".into(),
            read_only: true,
        }];
        let run = ContainerRun {
            name: "iunctus-run-P-1-1",
            image: &image,
            root: Path::new("/cache/scratch/run-P-1-1"),
            workdir: Some("/data"),
            mounts: &mounts,
            owner: (1000, 100),
            cpus: 2,
            memory: Some(1 << 30),
            offline: false,
            wrapper_path: None,
        };
        let limits = Limits {
            gpus: 1,
            seconds: Some(60),
        };
        let job = job("iunctus-run-p-1-1", "7-9", &[run], &limits);
        let spec = &job["spec"];
        assert_eq!(spec["backoffLimit"], 0);
        let pod = &spec["template"]["spec"];
        assert_eq!(pod["activeDeadlineSeconds"], 60);
        assert_eq!(pod["securityContext"]["runAsUser"], 1000);
        let container = &pod["containers"][0];
        assert_eq!(
            container["command"][1],
            "/cache/scratch/run-P-1-1/control/wrapper.sh"
        );
        assert_eq!(container["workingDir"], "/data");
        assert_eq!(container["resources"]["limits"]["memory"], "1073741824");
        assert_eq!(container["resources"]["limits"]["nvidia.com/gpu"], "1");
        let paths: Vec<(&str, &str)> = (pod["volumes"].as_array().expect("volumes").iter())
            .zip(container["volumeMounts"].as_array().expect("mounts"))
            .map(|(volume, mount)| {
                let path = volume["hostPath"]["path"].as_str().unwrap_or_default();
                (path, mount["mountPath"].as_str().unwrap_or_default())
            })
            .collect();
        let expected = [
            ("/cache/scratch/run-P-1-1", "/cache/scratch/run-P-1-1"),
            ("/cache/scratch/run-P-1-1/work", "/data"),
            ("/cache/store/cas", "/cache/store/cas"),
        ];
        assert_eq!(paths, expected);
        assert_eq!(job["spec"]["template"]["metadata"]["labels"][LABEL], "7-9");
    }
}
