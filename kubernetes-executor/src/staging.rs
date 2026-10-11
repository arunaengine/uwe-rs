//! How the pods of a backend get their files when the nodes share no folders with this machine:
//! through the iunctus helper and a plan.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::time::Duration;

use executor::capture::{RECORDS, capture};
use executor::process::Mount;
use executor::remote::RemoteStage;
use executor::wrapper::{Folders, wrapper};
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
}

/// What an attempt staged: where it sends its outputs back, and how its pod sees its folder.
#[derive(Debug)]
pub struct Prepared {
    pub prefix: String,
    /// What the container sees besides the folder of the attempt.
    pub mounts: Vec<Mount>,
    pub volumes: Volumes,
    /// The plan of the helper in the staging root.
    location: String,
}

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
        let Via::Helper(_) = &self.via;
        let transfer = text(&root.join(executor::helper::TRANSFER));
        let extra = [("control/wrapper.sh", capture(&script, &transfer)?)];
        // A task that runs as a user that the image names may differ from the helper.
        let open = spec.run_as.is_some_and(|run_as| run_as.user.is_none());
        let planned = self.stage.plan(job, root, &extra, RECORDS, open).await?;
        Ok(Prepared {
            prefix: planned.prefix,
            mounts: planned.mounts,
            volumes: Volumes::Under {
                root: root.to_owned(),
                source: json!({"emptyDir": {}}),
            },
            location: planned.location,
        })
    }

    /// Adds to the Job what its pod needs for staging: the helper.
    pub fn complete(&self, job: &mut Value, prepared: &Prepared, root: &Path) {
        let Via::Helper(helper) = &self.via;
        stage(job, helper, &root.to_string_lossy(), &prepared.location);
    }

    /// How long the pod may take to go away, when it does more than the task.
    pub fn grace(&self) -> Option<Duration> {
        None
    }

    /// Downloads what the attempt sent back into its folder `root`.
    pub async fn collect(&self, prepared: &Prepared, root: &Path) -> Result<(), ExecutorError> {
        self.stage.collect(&prepared.prefix, root, RECORDS).await
    }
}
