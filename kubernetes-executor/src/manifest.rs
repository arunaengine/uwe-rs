//! The Kubernetes Job of an attempt: one pod with one container, which runs the wrapper of the
//! task as the user, with the folders of the task at the same paths.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;

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

/// The `securityContext` of a container: the least permissions (decision 28), no
/// capabilities but those that its task needs and no way to gain privileges, as the user who
/// runs iunctus, or as Kubernetes runs it for a task that runs as its image says.
fn security(run: &ContainerRun<'_>) -> Value {
    let access = run.access;
    let mut context = json!({
        "allowPrivilegeEscalation": false,
        "privileged": false,
        "capabilities": {"drop": ["ALL"], "add": access.capabilities},
    });
    let (user, group) = match access.run_as {
        Some(run_as) => (run_as.user, run_as.group),
        None => (Some(run.owner.0), Some(run.owner.1)),
    };
    if let Some(user) = user {
        context["runAsUser"] = json!(user);
    }
    if let Some(group) = group {
        context["runAsGroup"] = json!(group);
    }
    context
}

/// What a Job asks for besides its container.
#[derive(Clone, Debug, Default)]
pub struct Limits {
    /// The GPUs of the task.
    pub gpus: u32,
    /// How long the task may run.
    pub seconds: Option<u64>,
}

/// Where the containers of a Job see the folders of their attempt.
#[derive(Clone, Debug, Default)]
pub enum Volumes {
    /// A `hostPath` volume for each folder: the nodes see the folders at the same paths.
    #[default]
    Host,
    /// One volume that holds every folder, below the folder `root` of this machine, mounted
    /// with a `subPath` for each folder: a shared claim or NFS export, an `emptyDir`, or a
    /// kubedal volume. `source` is the volume without its name.
    Under { root: PathBuf, source: Value },
}

/// The name of the volume of [`Volumes::Under`].
pub const FOLDERS: &str = "folders";

/// How the pods of object-store staging get the static iunctus binary: from an image that holds
/// only the binary, at `/iunctus`.
#[derive(Clone, Debug)]
pub struct Helper {
    pub image: String,
    /// Whether the task container sees the image as an image volume, which needs Kubernetes
    /// 1.31 or later with the `ImageVolume` feature; else an init container copies the binary.
    pub image_volume: bool,
    /// For nodes with credentials: the Secret whose keys the helper gets as environment
    /// variables, among them the storage providers of the staging root.
    pub secret: Option<String>,
}

/// Where the task container finds the helper.
const HELPER: &str = "/iunctus-helper";

/// Runs the wrapper and then sends the outputs back, with the helper `$1` and the folder `$2`
/// of the attempt. A failed upload is a failure of the machine.
const SEND: &str = "sh \"$2/control/wrapper.sh\"\nstatus=$?\n\"$1\" __stage out \"$2\" || exit 125\n\
                    exit \"$status\"\n";

/// Stages the files of a Job through the plan at `plan`: an init container of the helper image
/// downloads the inputs into the folder `root`, and the task container sends the outputs back
/// after the wrapper. The image of the task stays as it is.
pub fn stage(job: &mut Value, helper: &Helper, root: &str, plan: &str) {
    let pod = &mut job["spec"]["template"]["spec"];
    let task = pod["containers"][0].clone();
    // The init containers need no GPU of the task.
    let mut resources = task["resources"].clone();
    if let Some(limits) = resources["limits"].as_object_mut() {
        limits.remove("nvidia.com/gpu");
    }
    let base = json!({
        "image": helper.image,
        "resources": resources,
        "securityContext": task["securityContext"],
    });
    let binary = format!("{HELPER}/iunctus");
    let mut init = Vec::new();
    let volume = match helper.image_volume {
        true => json!({
            "name": "helper",
            "image": {"reference": helper.image, "pullPolicy": "IfNotPresent"},
        }),
        false => {
            let mut copy = base.clone();
            copy["name"] = json!("helper");
            copy["command"] = json!(["/iunctus", "__stage", "install", binary]);
            copy["volumeMounts"] = json!([{"name": "helper", "mountPath": HELPER}]);
            init.push(copy);
            json!({"name": "helper", "emptyDir": {}})
        }
    };
    let mut inputs = base;
    inputs["name"] = json!("stage-in");
    inputs["command"] = json!(["/iunctus", "__stage", "in", root, plan]);
    inputs["volumeMounts"] = json!([{"name": FOLDERS, "mountPath": root}]);
    let secret = (helper.secret.as_ref()).map(|name| json!([{"secretRef": {"name": name}}]));
    if let Some(secret) = &secret {
        inputs["envFrom"] = secret.clone();
    }
    init.push(inputs);
    pod["initContainers"] = json!(init);
    (pod["volumes"].as_array_mut())
        .expect("volumes")
        .push(volume);
    let container = &mut pod["containers"][0];
    container["command"] = json!(["sh", "-c", SEND, "iunctus", binary, root]);
    // The task container sends the outputs back, so it gets the Secret too.
    if let Some(secret) = secret {
        container["envFrom"] = secret;
    }
    let mount = json!({"name": "helper", "mountPath": HELPER, "readOnly": true});
    (container["volumeMounts"].as_array_mut())
        .expect("mounts")
        .push(mount);
}

/// Builds labelled Jobs with image-group containers sharing task folders and the task memory
/// limit. A folder outside the one volume of `place` is an error.
pub fn job(
    name: &str,
    label: &str,
    runs: &[ContainerRun<'_>],
    limits: &Limits,
    place: &Volumes,
) -> Result<Value, String> {
    let run = &runs[0];
    let mut mounts = Vec::new();
    let mut volumes = Vec::new();
    if let Volumes::Under { source, .. } = place {
        let mut volume = source.clone();
        volume["name"] = json!(FOLDERS);
        volumes.push(volume);
    }
    for (index, bind) in run.binds().iter().enumerate() {
        let Volumes::Under { root, .. } = place else {
            let volume = format!("folder-{index}");
            mounts.push(json!({
                "name": volume,
                "mountPath": bind.target,
                "readOnly": bind.read_only,
            }));
            let source = bind.source.to_string_lossy();
            volumes.push(json!({"name": volume, "hostPath": {"path": source}}));
            continue;
        };
        let inside = bind.source.strip_prefix(root).map_err(|_| {
            format!(
                "{} is not in the folder {}",
                bind.source.display(),
                root.display()
            )
        })?;
        let mut mount = json!({
            "name": FOLDERS,
            "mountPath": bind.target,
            "readOnly": bind.read_only,
        });
        if !inside.as_os_str().is_empty() {
            mount["subPath"] = json!(inside.to_string_lossy());
        }
        mounts.push(mount);
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
            let mut container = json!({
                "name": name,
                "image": run.image.reference,
                "command": ["sh", run.script()],
                "resources": {"requests": requests, "limits": limits_of_container},
                "volumeMounts": mounts,
                "securityContext": security(run),
            });
            if let Some(start) = run.start() {
                container["workingDir"] = json!(start);
            }
            container
        })
        .collect();
    let mut spec = json!({
        "backoffLimit": 0,
        "ttlSecondsAfterFinished": KEEP_SECONDS,
        "template": {
            "metadata": {"labels": labels},
            "spec": {
                "restartPolicy": "Never",
                "automountServiceAccountToken": false,
                "containers": containers,
                "volumes": volumes,
            },
        },
    });
    // The time counts on the pod, from its start, so that waiting for a node does not count.
    if let Some(seconds) = limits.seconds {
        spec["template"]["spec"]["activeDeadlineSeconds"] = json!(seconds);
    }
    Ok(json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {"name": name, "labels": labels},
        "spec": spec,
    }))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use executor::Access;
    use executor::process::Mount;
    use task_model::Image;

    use super::*;

    #[test]
    fn names_are_labels() {
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
    fn jobs_mount_folders() {
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
            access: &Access::of((1000, 100), false, None, ""),
        };
        let limits = Limits {
            gpus: 1,
            seconds: Some(60),
        };
        let job = job("iunctus-run-p-1-1", "7-9", &[run], &limits, &Volumes::Host).unwrap();
        let spec = &job["spec"];
        assert_eq!(spec["backoffLimit"], 0);
        let pod = &spec["template"]["spec"];
        assert_eq!(pod["activeDeadlineSeconds"], 60);
        assert_eq!(pod["automountServiceAccountToken"], false);
        let container = &pod["containers"][0];
        let security = &container["securityContext"];
        assert_eq!(security["runAsUser"], 1000);
        assert_eq!(security["allowPrivilegeEscalation"], false);
        assert_eq!(security["capabilities"]["drop"][0], "ALL");
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
        // A container that runs as its image says starts in its `workdir`, as root with only
        // CHOWN, and the work directory stays in the scratch directory.
        let access = Access::of((1000, 100), false, Some(task_model::RunAs::default()), "");
        let run = ContainerRun {
            access: &access,
            ..run
        };
        let changed = super::job("iunctus-run-p-1-1", "7-9", &[run], &limits, &Volumes::Host);
        let changed = changed.unwrap();
        let container = &changed["spec"]["template"]["spec"]["containers"][0];
        assert_eq!(container["workingDir"], "/data");
        assert_eq!(
            container["securityContext"]["capabilities"]["add"][0],
            "CHOWN"
        );
        assert!(container["securityContext"].get("runAsUser").is_none());
        let mounts = container["volumeMounts"].as_array().expect("mounts");
        assert!(!mounts.iter().any(|mount| mount["mountPath"] == "/data"));
    }

    #[test]
    fn mounts_one_volume() {
        let image = Image {
            reference: "debian@sha256:ab".into(),
            platform: None,
        };
        let mounts = [Mount {
            source: PathBuf::from("/cache/store/cas"),
            target: "/cache/store/cas".into(),
            read_only: true,
        }];
        let access = Access::of((1000, 100), false, None, "");
        let run = ContainerRun {
            name: "iunctus-run-P-1-1",
            image: &image,
            root: Path::new("/cache/scratch/run-P-1-1"),
            workdir: Some("/data"),
            mounts: &mounts,
            owner: (1000, 100),
            cpus: 1,
            memory: None,
            offline: false,
            wrapper_path: None,
            access: &access,
        };
        let claim = Volumes::Under {
            root: PathBuf::from("/cache"),
            source: json!({"persistentVolumeClaim": {"claimName": "iunctus"}}),
        };
        let limits = Limits::default();
        let manifest = job("iunctus-run-p-1-1", "7-9", &[run], &limits, &claim).unwrap();
        let pod = &manifest["spec"]["template"]["spec"];
        let volumes = pod["volumes"].as_array().expect("volumes");
        assert_eq!(volumes.len(), 1);
        assert_eq!(volumes[0]["name"], FOLDERS);
        assert_eq!(volumes[0]["persistentVolumeClaim"]["claimName"], "iunctus");
        let mounts: Vec<(&str, &str)> = (pod["containers"][0]["volumeMounts"].as_array())
            .expect("mounts")
            .iter()
            .map(|mount| {
                let path = mount["subPath"].as_str().unwrap_or_default();
                (path, mount["mountPath"].as_str().unwrap_or_default())
            })
            .collect();
        let expected = [
            ("scratch/run-P-1-1", "/cache/scratch/run-P-1-1"),
            ("scratch/run-P-1-1/work", "/data"),
            ("store/cas", "/cache/store/cas"),
        ];
        assert_eq!(mounts, expected);
        let elsewhere = Volumes::Under {
            root: PathBuf::from("/shared"),
            source: json!({"emptyDir": {}}),
        };
        let error = job("iunctus-run-p-1-1", "7-9", &[run], &limits, &elsewhere);
        assert!(error.unwrap_err().contains("is not in the folder /shared"));
    }
}
