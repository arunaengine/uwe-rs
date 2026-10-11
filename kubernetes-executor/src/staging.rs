//! How the pods of a backend get their files when the nodes share no folders with this machine:
//! through the iunctus helper and a plan, or through kubedal volumes.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::time::Duration;

use executor::capture::{RECORDS, capture};
use executor::process::Mount;
use executor::remote::RemoteStage;
use executor::wrapper::{Folders, quote, wrapper};
use executor::{CONTROL, ContainerRun, ExecutorError, HOME, Job};
use serde_json::{Value, json};
use task_model::TaskSpec;

use crate::manifest::{Helper, Volumes, stage};

/// The staging root of a backend, and how its pods reach it.
#[derive(Clone, Debug)]
pub struct PodStaging {
    pub stage: RemoteStage,
    pub via: Via,
}

/// How the pods reach the staging root.
#[derive(Clone, Debug)]
pub enum Via {
    /// The iunctus helper, from an image that holds only the binary, with presigned requests.
    Helper(Helper),
    /// The kubedal CSI driver, which places the inputs and uploads the outputs after the pod.
    Kubedal(Kubedal),
}

/// The kubedal setup that the staging root needs.
#[derive(Clone, Debug)]
pub struct Kubedal {
    /// The name of the CSI driver, like `kubedal.arunaengine.org`.
    pub driver: String,
    /// The DataNode whose folder is the staging root.
    pub data_node: String,
    /// The namespace of the DataNode; `None` for the namespace of the Jobs.
    pub namespace: Option<String>,
    /// How long kubedal may take to upload the outputs after the pod ended.
    pub upload: Duration,
}

/// What an attempt staged: where it sends its outputs back, and how its pod sees its folder.
#[derive(Debug)]
pub struct Prepared {
    pub prefix: String,
    /// What the container sees besides the folder of the attempt.
    pub mounts: Vec<Mount>,
    pub volumes: Volumes,
    /// Where the node reads the plan of the helper, or the file list of kubedal.
    location: String,
    /// The plan or the file list in the staging root, in `cas/`.
    plan: String,
    /// The group that kubedal gives the files of the volume, so that the task may change them.
    group: Option<u32>,
    /// Where the pod downloads the binary of [`crate::Helper::binary`].
    binary: Option<String>,
}

/// The folder of the attempt that kubedal uploads to the attempt prefix.
const UPLOAD: &str = "upload";

impl PodStaging {
    /// Uploads the inputs and the captured wrapper of `spec`, which runs as `run`.
    pub async fn prepare(
        &self,
        job: Job<'_>,
        run: &ContainerRun<'_>,
        spec: &TaskSpec,
    ) -> Result<Prepared, ExecutorError> {
        let root = run.root;
        let text = |path: &Path| path.to_string_lossy().into_owned();
        let (control, home) = (text(&root.join(CONTROL)), text(&root.join(HOME)));
        let (outbox, owner) = (text(&root.join("outbox")), run.access.owner());
        let folders = Folders {
            work: &run.inside(),
            control: &control,
            home: &home,
            outbox: &outbox,
            owner: &owner,
        };
        let script = wrapper(spec, &folders);
        let kubedal = match &self.via {
            Via::Helper(helper) => {
                let binary = match &helper.binary {
                    Some(binary) => Some(self.stage.binary(binary).await?),
                    None => None,
                };
                let transfer = text(&root.join(executor::helper::TRANSFER));
                let extra = [("control/wrapper.sh", capture(&script, &transfer)?)];
                // A task that runs as a user that the image names may differ from the helper.
                let open = spec.run_as.is_some_and(|run_as| run_as.user.is_none());
                let planned = self.stage.plan(job, root, &extra, RECORDS, open).await?;
                return Ok(Prepared {
                    prefix: planned.prefix,
                    mounts: planned.mounts,
                    volumes: Volumes::Under {
                        root: root.to_owned(),
                        source: json!({"emptyDir": {}}),
                    },
                    location: planned.location,
                    plan: planned.plan,
                    group: None,
                    binary,
                });
            }
            Via::Kubedal(kubedal) => kubedal,
        };
        let files = text(&root.join(UPLOAD).join("files"));
        let records = text(&root.join(UPLOAD).join(CONTROL));
        let script = capture(&script, &files)?;
        let script = script
            .strip_suffix("exit \"$status\"\n")
            .expect("a final exit");
        let names: Vec<String> = RECORDS.iter().map(|name| quote(name)).collect();
        let script = format!(
            "{script}mkdir -p {records}\nfor name in {}; do\n    [ ! -f \"$control/$name\" ] || \
             cp \"$control/$name\" {records}/\ndone\nexit \"$status\"\n",
            names.join(" "),
            records = quote(&records),
        );
        let prefix = executor::capture::attempt_prefix(job)?;
        let layout = (self.stage)
            .upload(job, root, &[("control/wrapper.sh", script)])
            .await?;
        let mut directories = layout.directories;
        directories.push(UPLOAD.into());
        let files: Vec<Value> = (layout.files.iter())
            .map(|file| {
                json!({
                    "path": file.path,
                    "source": content_store::layout::content(&file.digest),
                    "sha256": file.digest.hex(),
                    "size": file.size,
                    "executable": file.executable,
                })
            })
            .collect();
        let mut upload = json!({"dataNode": kubedal.data_node, "prefix": prefix, "path": UPLOAD});
        let mut attributes = json!({"dataNode": kubedal.data_node});
        if let Some(namespace) = &kubedal.namespace {
            upload["dataNodeNamespace"] = json!(namespace);
            attributes["dataNodeNamespace"] = json!(namespace);
        }
        let list = json!({
            "version": 1,
            "files": files,
            "directories": directories,
            "upload": upload,
        });
        let text = serde_json::to_vec(&list).map_err(failed)?;
        let digest = (self.stage.objects.put_content(text.into()).await).map_err(failed)?;
        let location = content_store::layout::content(&digest);
        attributes["manifest"] = json!(location);
        let plan = location.clone();
        let source = json!({"csi": {"driver": kubedal.driver, "volumeAttributes": attributes}});
        Ok(Prepared {
            prefix,
            mounts: layout.mounts,
            volumes: Volumes::Under {
                root: root.to_owned(),
                source,
            },
            location,
            plan,
            group: match run.access.run_as {
                None => Some(run.owner.1),
                Some(run_as) => run_as.group,
            },
            binary: None,
        })
    }

    /// Adds to the Job what its pod needs for staging: the helper, or the group of kubedal.
    pub fn complete(&self, job: &mut Value, prepared: &Prepared, root: &Path) {
        match &self.via {
            Via::Helper(helper) => {
                let (root, binary) = (root.to_string_lossy(), prepared.binary.as_deref());
                stage(job, helper, &root, &prepared.location, binary)
            }
            Via::Kubedal(_) => {
                if let Some(group) = prepared.group {
                    job["spec"]["template"]["spec"]["securityContext"] = json!({"fsGroup": group});
                }
            }
        }
    }

    /// How long the pod may take to go away: kubedal uploads the outputs on the way.
    pub fn grace(&self) -> Option<Duration> {
        match &self.via {
            Via::Helper(_) => None,
            Via::Kubedal(kubedal) => Some(kubedal.upload),
        }
    }

    /// Downloads what the attempt sent back into its folder `root`, and then removes it from
    /// the staging root if the stage cleans up.
    pub async fn collect(&self, prepared: &Prepared, root: &Path) -> Result<(), ExecutorError> {
        self.stage.collect(&prepared.prefix, root, RECORDS).await?;
        self.stage.clean(&prepared.prefix, &prepared.plan).await;
        Ok(())
    }
}

fn failed(error: impl std::fmt::Display) -> ExecutorError {
    std::io::Error::other(format!("kubedal staging failed: {error}")).into()
}
