//! The Kubernetes Job of an attempt: one pod with one container, which runs the wrapper of the
//! task as the user, with the folders of the task at the same paths.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;

use serde_json::{Value, json};

use executor::ContainerRun;
use executor::binary::Binary;
use executor::wrapper::timed;

/// The label that marks the Jobs of one backend, so that one request lists all their pods.
pub const LABEL: &str = "uwe.dev/attempts";

/// How long Kubernetes keeps a Job that ended, in case the backend could not delete it.
const KEEP_SECONDS: u64 = 600;

/// The name of the Job of an attempt: a DNS label of at most 63 lowercase letters, digits and
/// dashes. A long name keeps its start and gets a digest of the whole name, so that names
/// stay unique.
pub fn job_name(name: &str) -> String {
    let mut clean: String = format!("uwe-{name}")
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
    /// How much longer than its task the pod may run: for image pulls, staging and the start of
    /// its containers.
    pub margin: u64,
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
/// only the binary, at `/uwe`.
#[derive(Clone, Debug)]
pub struct Helper {
    pub image: String,
    /// Whether the task container sees the image as an image volume, which needs Kubernetes
    /// 1.31 or later with the `ImageVolume` feature; else an init container copies the binary.
    pub image_volume: bool,
    /// For nodes with credentials: the Secret whose keys the helper gets as environment
    /// variables, among them the storage providers of the staging root.
    pub secret: Option<String>,
    /// A binary of this machine, which an init container of `image` downloads from the staging
    /// root instead; `image` then needs a shell, `wget` and `sha256sum`, like Alpine.
    pub binary: Option<Binary>,
}

/// The image that downloads the binary of [`Helper::binary`]: Alpine, pinned to its digest. Its
/// `wget` checks the certificate of the server, which that of busybox does not do.
pub const DOWNLOAD_IMAGE: &str = "docker.io/library/alpine:3.24.2@sha256:\
                                  294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6";

/// Downloads the binary `$1` from the address `$2` and checks its SHA-256 `$3`.
const DOWNLOAD: &str = "wget -q -O \"$1\" \"$2\" && echo \"$3  $1\" | sha256sum -c - > /dev/null \
                        && chmod +x \"$1\"";

/// Where the task container finds the helper.
const HELPER: &str = "/uwe-helper";

/// Runs the wrapper and then sends the outputs back, with the helper `$1` and the folder `$2`
/// of the attempt. A failed upload is a failure of the machine.
const SEND: &str = "\"$1\" __stage out \"$2\" || exit 125\nexit \"$status\"\n";

/// Stages the files of a Job through the plan at `plan`: an init container fills the folder
/// `root`, and the task container sends the outputs back after the wrapper, which keeps the time
/// `seconds` of its task. `binary` is where the pod downloads the binary of [`Helper::binary`].
pub fn stage(
    job: &mut Value,
    helper: &Helper,
    root: &str,
    plan: &str,
    binary: Option<&str>,
    seconds: Option<u64>,
) {
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
    let address = binary;
    let binary = format!("{HELPER}/uwe");
    let mut init = Vec::new();
    let mut program = "/uwe".to_owned();
    let volume = match (&helper.binary, helper.image_volume) {
        (Some(uploaded), _) => {
            let mut download = base.clone();
            download["name"] = json!("helper");
            let address = address.expect("the address of the uploaded binary");
            let sha256 = uploaded.digest.hex();
            download["command"] = json!(["sh", "-c", DOWNLOAD, "sh", binary, address, sha256]);
            download["volumeMounts"] = json!([{"name": "helper", "mountPath": HELPER}]);
            init.push(download);
            program = binary.clone();
            // The binary runs only on the nodes of its own machine.
            let arch = uploaded.machine.kubernetes();
            pod["nodeSelector"] = json!({"kubernetes.io/arch": arch});
            json!({"name": "helper", "emptyDir": {}})
        }
        (None, true) => json!({
            "name": "helper",
            "image": {"reference": helper.image, "pullPolicy": "IfNotPresent"},
        }),
        (None, false) => {
            let mut copy = base.clone();
            copy["name"] = json!("helper");
            copy["command"] = json!(["/uwe", "__stage", "install", binary]);
            copy["volumeMounts"] = json!([{"name": "helper", "mountPath": HELPER}]);
            init.push(copy);
            json!({"name": "helper", "emptyDir": {}})
        }
    };
    let mut inputs = base;
    inputs["name"] = json!("stage-in");
    inputs["command"] = json!([program, "__stage", "in", root, plan]);
    inputs["volumeMounts"] = json!([{"name": FOLDERS, "mountPath": root}]);
    if helper.binary.is_some() {
        let mount = json!({"name": "helper", "mountPath": HELPER, "readOnly": true});
        (inputs["volumeMounts"].as_array_mut())
            .expect("mounts")
            .push(mount);
    }
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
    let send = format!("{}{SEND}", timed("sh \"$2/control/wrapper.sh\"", seconds));
    container["command"] = json!(["sh", "-c", send, "uwe", binary, root]);
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
    let labels = json!({"app.kubernetes.io/managed-by": "uwe", LABEL: label});
    let containers: Vec<Value> = (runs.iter().enumerate())
        .map(|(index, run)| {
            let (name, requests) = match (runs.len(), index) {
                (1, _) => ("task".to_owned(), requests.clone()),
                (_, 0) => ("task-0".to_owned(), requests.clone()),
                (_, index) => (format!("task-{index}"), json!({"cpu": "0", "memory": "0"})),
            };
            let command = match limits.seconds {
                None => json!(["sh", run.script()]),
                Some(_) => {
                    let text = format!("{}exit \"$status\"\n", timed("sh \"$1\"", limits.seconds));
                    json!(["sh", "-c", text, "uwe", run.script()])
                }
            };
            let mut container = json!({
                "name": name,
                "image": run.image.reference,
                "command": command,
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
    // Image pulls and staging count too, so the pod gets a margin and the wrapper the time.
    if let Some(seconds) = limits.seconds {
        let deadline = seconds.saturating_add(limits.margin);
        spec["template"]["spec"]["activeDeadlineSeconds"] = json!(deadline);
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
        assert_eq!(job_name("1a0f-49038-UPPER-1-1"), "uwe-1a0f-49038-upper-1-1");
        let long = job_name(&format!("run-{}-1", "NFCORE_DEMO:DEMO:FASTQC".repeat(4)));
        assert!(long.len() <= 63, "{long}");
        assert!(
            long.starts_with("uwe-run-nfcore-demo-demo-fastqc"),
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
            name: "uwe-run-P-1-1",
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
            margin: 300,
        };
        let job = job("uwe-run-p-1-1", "7-9", &[run], &limits, &Volumes::Host).unwrap();
        let spec = &job["spec"];
        assert_eq!(spec["backoffLimit"], 0);
        let pod = &spec["template"]["spec"];
        // The pod gets a margin for staging, and the wrapper stops at the time of the task.
        assert_eq!(pod["activeDeadlineSeconds"], 360);
        assert_eq!(pod["automountServiceAccountToken"], false);
        let container = &pod["containers"][0];
        let security = &container["securityContext"];
        assert_eq!(security["runAsUser"], 1000);
        assert_eq!(security["allowPrivilegeEscalation"], false);
        assert_eq!(security["capabilities"]["drop"][0], "ALL");
        assert_eq!(
            container["command"][4],
            "/cache/scratch/run-P-1-1/control/wrapper.sh"
        );
        let text = container["command"][2].as_str().expect("a script");
        assert!(text.starts_with("sh \"$1\" &\n"), "{text}");
        assert!(text.contains("( sleep 60 && kill -TERM \"$pid\""), "{text}");
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
        let changed = super::job("uwe-run-p-1-1", "7-9", &[run], &limits, &Volumes::Host);
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
    fn downloads_uploaded_helper() {
        let image = Image {
            reference: "debian@sha256:ab".into(),
            platform: None,
        };
        let access = Access::of((1000, 100), false, None, "");
        let run = ContainerRun {
            name: "uwe-run-P-1-1",
            image: &image,
            root: Path::new("/cache/scratch/run-P-1-1"),
            workdir: None,
            mounts: &[],
            owner: (1000, 100),
            cpus: 1,
            memory: None,
            offline: false,
            wrapper_path: None,
            access: &access,
        };
        let place = Volumes::Under {
            root: PathBuf::from("/cache/scratch/run-P-1-1"),
            source: json!({"emptyDir": {}}),
        };
        let limits = Limits {
            gpus: 1,
            seconds: None,
            margin: 0,
        };
        let mut manifest = job("uwe-run-p-1-1", "7-9", &[run], &limits, &place).unwrap();
        let digest = task_model::Digest::sha256(b"uwe");
        let helper = Helper {
            image: DOWNLOAD_IMAGE.into(),
            image_volume: true,
            secret: None,
            binary: Some(Binary {
                file: PathBuf::from("/usr/bin/uwe"),
                digest: digest.clone(),
                machine: executor::binary::Machine::Amd64,
                uploaded: Default::default(),
            }),
        };
        let (root, url) = ("/cache/scratch/run-P-1-1", Some("https://staging/cas/x"));
        stage(&mut manifest, &helper, root, "https://plan", url, Some(30));
        let pod = &manifest["spec"]["template"]["spec"];
        assert_eq!(pod["nodeSelector"]["kubernetes.io/arch"], "amd64");
        let download = &pod["initContainers"][0];
        assert_eq!(download["image"], DOWNLOAD_IMAGE);
        // The presigned address must not go to a server whose certificate is not checked.
        assert!(DOWNLOAD_IMAGE.starts_with("docker.io/library/alpine:"));
        let command = json!([
            "sh",
            "-c",
            DOWNLOAD,
            "sh",
            "/uwe-helper/uwe",
            "https://staging/cas/x",
            digest.hex(),
        ]);
        assert_eq!(download["command"], command);
        // The init containers run as the task, without its GPU.
        assert!(
            download["resources"]["limits"]
                .get("nvidia.com/gpu")
                .is_none()
        );
        assert_eq!(download["securityContext"]["runAsUser"], 1000);
        let stage_in = &pod["initContainers"][1];
        assert_eq!(stage_in["image"], DOWNLOAD_IMAGE);
        let command = json!(["/uwe-helper/uwe", "__stage", "in", root, "https://plan"]);
        assert_eq!(stage_in["command"], command);
        assert_eq!(stage_in["volumeMounts"][1]["mountPath"], "/uwe-helper");
        let volumes = pod["volumes"].as_array().expect("volumes");
        assert_eq!(volumes[1], json!({"name": "helper", "emptyDir": {}}));
        let task = &pod["containers"][0];
        assert_eq!(task["image"], "debian@sha256:ab");
        assert_eq!(task["command"][4], "/uwe-helper/uwe");
        let send = task["command"][2].as_str().expect("a script");
        assert!(send.contains("( sleep 30 && kill"), "{send}");
        assert!(send.ends_with("__stage out \"$2\" || exit 125\nexit \"$status\"\n"));
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
            name: "uwe-run-P-1-1",
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
            source: json!({"persistentVolumeClaim": {"claimName": "uwe"}}),
        };
        let limits = Limits::default();
        let manifest = job("uwe-run-p-1-1", "7-9", &[run], &limits, &claim).unwrap();
        let pod = &manifest["spec"]["template"]["spec"];
        let volumes = pod["volumes"].as_array().expect("volumes");
        assert_eq!(volumes.len(), 1);
        assert_eq!(volumes[0]["name"], FOLDERS);
        assert_eq!(volumes[0]["persistentVolumeClaim"]["claimName"], "uwe");
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
        let error = job("uwe-run-p-1-1", "7-9", &[run], &limits, &elsewhere);
        assert!(error.unwrap_err().contains("is not in the folder /shared"));
    }
}
