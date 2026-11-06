//! Runs task attempts as Google Batch jobs: stages through the staging root, creates the job,
//! polls it, deletes a cancelled one, and collects what it sent back.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use executor::batch::{Helper, Source};
use executor::binary::Binary;
use executor::capture::{RECORDS, capture};
use executor::registry::{self, ImageConfigs};
use executor::remote::RemoteStage;
use executor::wrapper::{Folders, wrapper};
use executor::{Access, Backend, CONTROL, ContainerRun, ExecutorError, Finished, HOME, Job};
use task_model::TaskSpec;
use tokio_util::sync::CancellationToken;

use crate::client::{Client, GoogleError};
use crate::job::{Download, Shape};
use crate::outcome::{Ended, finish, machine};
use crate::settings::Settings;

/// What Google Batch writes in the last event of a job that waits for quota.
const QUOTA: &str = "CODE_GCE_QUOTA_EXCEEDED";

/// How many polls in a row may miss a job before it counts as lost.
const MISSES: u32 = 30;

/// How many lines of the end of the log of a failed job go into its error.
const LOG_LINES: u32 = 30;

/// The Google Batch executor's backend.
#[derive(Debug)]
pub struct GoogleBackend {
    pub client: Client,
    pub settings: Settings,
    /// Where each attempt gets the local folder that its results come back to.
    pub scratch: PathBuf,
    /// The staging root, which this machine and the jobs reach.
    pub stage: RemoteStage,
    /// The binary that uwe uploads for the jobs, when the settings name no helper.
    pub upload: Option<Binary>,
    /// How much longer than its task a job may run: for its image pull and staging.
    pub staging_time: Duration,
    /// The entrypoints and default commands of images, for tasks without a command.
    pub configs: ImageConfigs,
    held: AtomicBool,
}

impl GoogleBackend {
    /// A backend whose jobs get the helper as `settings` say, or else `upload` from the
    /// staging root through the image of `settings.download_image`.
    pub fn new(
        client: Client,
        settings: Settings,
        scratch: PathBuf,
        stage: RemoteStage,
        upload: Option<Binary>,
    ) -> Result<GoogleBackend, String> {
        if settings.helper.is_none() && upload.is_none() {
            return Err(
                "Google Batch jobs need the uwe helper: set helper_path, helper_url or \
                 helper_file in [executor.google-batch]"
                    .into(),
            );
        }
        let downloads = !matches!(settings.helper, Some(Helper::Path(_)));
        if downloads && settings.download_image.is_none() {
            return Err("Google Batch jobs need helper_download_image to get the helper".into());
        }
        if stage.credentials {
            return Err(
                "Google Batch staging uses presigned requests; leave out staging_access".into(),
            );
        }
        Ok(GoogleBackend {
            client,
            settings,
            scratch,
            stage,
            upload,
            staging_time: Duration::from_secs(30 * 60),
            configs: ImageConfigs::default(),
            held: AtomicBool::new(false),
        })
    }
}

impl Backend for GoogleBackend {
    async fn pin(&self, image: &str) -> Result<Option<String>, ExecutorError> {
        registry::pin(image).await
    }

    fn hold(&self) {
        self.held.store(true, Ordering::Relaxed);
    }

    async fn run(
        &self,
        job: Job<'_>,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        if cancel.is_cancelled() || self.held.load(Ordering::Relaxed) {
            return Err(ExecutorError::Cancelled);
        }
        let mut spec = job.spec.clone();
        admit(job, &spec)?;
        let image = spec.image.as_ref().ok_or(ExecutorError::NoImage)?;
        if spec.arguments.is_empty() {
            // Google Batch runs the entrypoint of the image around the commands of the job.
            let config = self.configs.get(&image.reference).await?;
            spec.arguments = config.cmd.clone();
        }
        if spec.arguments.is_empty() {
            return Err(ExecutorError::NoCommand(image.reference.clone()));
        }
        let local = self.scratch.join(job.name);
        tokio::fs::create_dir_all(&self.scratch).await?;
        // Another attempt with this name owns its folder.
        tokio::fs::create_dir(&local).await?;
        let mut staged = None;
        let result = self.attempt(job, &spec, &local, &mut staged, cancel).await;
        if let Some((prefix, plan)) = &staged {
            self.stage.clean(prefix, plan).await;
        }
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&local).await;
        }
        result
    }
}

/// Refuses what a job of Google Batch cannot do, before anything is uploaded.
fn admit(job: Job<'_>, spec: &TaskSpec) -> Result<(), ExecutorError> {
    let refuse = |message: &str| Err(ExecutorError::Unsupported(message.to_owned()));
    if job.placement.offline {
        return refuse("Google Batch cannot run a task without network, as it stages its files");
    }
    if !spec.containers.is_empty() {
        return refuse("Google Batch runs only tasks of one container");
    }
    if spec.run_as.is_some() {
        return refuse("Google Batch runs a task as the user of its image; it cannot set one");
    }
    let absolute = (job.staging.inputs.iter().map(|input| &input.path))
        .chain(&job.staging.directories)
        .any(|path| path.starts_with('/'));
    if absolute {
        return refuse("Google Batch cannot place inputs at absolute paths in the container");
    }
    Ok(())
}

impl GoogleBackend {
    /// Stages an attempt, creates its job and waits for its end. The objects of the attempt go
    /// to `staged` as soon as they are in the staging root.
    async fn attempt(
        &self,
        job: Job<'_>,
        spec: &TaskSpec,
        local: &Path,
        staged: &mut Option<(String, String)>,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        let image = spec.image.as_ref().ok_or(ExecutorError::NoImage)?;
        let folder = &self.settings.task_folder;
        let root = Path::new(folder).join(job.name);
        let access = Access::default();
        let run = ContainerRun {
            name: job.name,
            image,
            root: &root,
            workdir: spec.workdir.as_deref(),
            mounts: &[],
            owner: (0, 0),
            cpus: job.placement.cpus.max(1),
            memory: job.placement.memory_bytes,
            offline: false,
            wrapper_path: None,
            access: &access,
        };
        let (control, home) = (text(&root.join(CONTROL)), text(&root.join(HOME)));
        let folders = Folders {
            work: &run.inside(),
            control: &control,
            home: &home,
            outbox: &text(&root.join("outbox")),
            owner: "",
        };
        let transfer = text(&root.join(executor::helper::TRANSFER));
        let script = capture(&wrapper(spec, &folders), &transfer)?;
        let discovery = executor::helper::Discovery::of(spec, job.staging);
        let extra = [("control/wrapper.sh", script)];
        let planned = (self.stage)
            .plan_discovery(job, &root, &extra, &[], RECORDS, false, discovery)
            .await?;
        *staged = Some((planned.prefix.clone(), planned.plan.clone()));
        // A downloaded helper lies next to the folder of the attempt, which both runnables see.
        let fetched = format!("{}.uwe", text(&root));
        let mut volumes = vec![format!("{folder}:{folder}")];
        let uploaded = match (&self.settings.helper, &self.upload) {
            (None, Some(binary)) => Some((self.stage.binary(binary).await?, binary)),
            _ => None,
        };
        let download_image = self.settings.download_image.as_deref().unwrap_or_default();
        let (source, download) = match (&self.settings.helper, &uploaded) {
            (Some(Helper::Path(path)), _) => {
                let parent = match path.rsplit_once('/') {
                    Some(("", _)) | None => "/",
                    Some((parent, _)) => parent,
                };
                volumes.push(format!("{parent}:{parent}:ro"));
                (Source::Path(path), None)
            }
            (Some(Helper::Download { url, sha256 }), _) => {
                let download = Download {
                    image: download_image,
                    path: &fetched,
                    url,
                    sha256,
                    machine: None,
                };
                (Source::Path(&fetched), Some(download))
            }
            (None, Some((url, binary))) => {
                let download = Download {
                    image: download_image,
                    path: &fetched,
                    url,
                    sha256: binary.digest.hex(),
                    machine: Some(binary.machine.uname()),
                };
                (Source::Path(&fetched), Some(download))
            }
            (None, None) => {
                return Err(ExecutorError::Unsupported(
                    "no uwe helper for the jobs".into(),
                ));
            }
        };
        let time = job.placement.time_seconds;
        let commands = executor::batch::command(&text(&root), &planned.location, source, time);
        let shape = Shape {
            task: job.name,
            image: &image.reference,
            commands,
            volumes,
            download,
            placement: job.placement,
            seconds: time.map(|time| time + self.staging_time.as_secs()),
        };
        let body = crate::job::body(&shape, &self.settings).map_err(ExecutorError::Unsupported)?;
        let millis = (SystemTime::now().duration_since(UNIX_EPOCH))
            .map(|elapsed| elapsed.as_millis())
            .unwrap_or_default();
        let id = crate::job::job_id(job.name, millis);
        if cancel.is_cancelled() || self.held.load(Ordering::Relaxed) {
            return Err(ExecutorError::Cancelled);
        }
        let created = self.client.create(&id, &body).await.map_err(api)?;
        let uid = created["uid"].as_str().unwrap_or_default().to_owned();
        tracing::debug!(job = id, attempt = job.name, "[GOOGLE BATCH] created");
        let ended = self.wait(&id, job.name, cancel).await?;
        let collected =
            executor::remote::patiently(|| self.stage.collect(&planned.prefix, local, RECORDS))
                .await;
        if let Err(error) = collected {
            // The objects stay, as they are the only copy of the results.
            *staged = None;
            return Err(error);
        }
        match finish(&ended, local.to_owned(), time, spec).await {
            Err(ExecutorError::Engine { engine, message }) => {
                let log = self.log(&uid, &id).await;
                Err(ExecutorError::Engine {
                    engine,
                    message: format!("{message}{log}"),
                })
            }
            finished => finished,
        }
    }

    /// Polls the job until it ended, and deletes it when `cancel` fires.
    async fn wait(
        &self,
        id: &str,
        attempt: &str,
        cancel: &CancellationToken,
    ) -> Result<Ended, ExecutorError> {
        let (mut misses, mut warned) = (0, false);
        loop {
            tokio::select! {
                () = tokio::time::sleep(self.settings.poll) => {}
                () = cancel.cancelled() => {
                    self.delete(id).await;
                    return Err(ExecutorError::Cancelled);
                }
            }
            let seen = match self.client.job(id).await {
                Ok(seen) => seen,
                Err(error) => {
                    self.delete(id).await;
                    return Err(api(error));
                }
            };
            let Some(seen) = seen else {
                misses += 1;
                if misses >= MISSES {
                    return Err(machine(format!(
                        "Google Batch does not know the job {id} any more"
                    )));
                }
                continue;
            };
            misses = 0;
            if let Some(ended) = crate::outcome::ended(&seen) {
                return Ok(ended);
            }
            let status = &seen["status"];
            let last = (status["statusEvents"].as_array())
                .and_then(|events| events.last())
                .and_then(|event| event["description"].as_str())
                .unwrap_or_default();
            match status["state"].as_str().unwrap_or_default() {
                "DELETION_IN_PROGRESS" => {
                    return Err(machine(format!("Google Batch deletes the job {id}")));
                }
                "SCHEDULED" | "QUEUED" if last.contains(QUOTA) && !warned => {
                    tracing::warn!(job = id, attempt, "Batch job cannot be run: {last}");
                    warned = true;
                }
                _ => {}
            }
        }
    }

    /// Deletes a job, which stops it; a failure leaves it running, so it is a warning.
    async fn delete(&self, id: &str) {
        if let Err(error) = self.client.delete(id).await {
            tracing::warn!("[GOOGLE BATCH] Unable to delete job {id} -- {error}");
        }
    }

    /// The end of the Cloud Logging log of a job, as a note for its error; empty when there is
    /// none.
    async fn log(&self, uid: &str, id: &str) -> String {
        if uid.is_empty() || self.settings.logs_path.is_some() {
            return String::new();
        }
        match self.client.log_tail(uid, LOG_LINES).await {
            Ok(lines) if !lines.is_empty() => format!("\n{}", lines.join("\n")),
            Ok(_) => String::new(),
            Err(error) => {
                tracing::debug!(job = id, %error, "cannot read the log of the job");
                String::new()
            }
        }
    }
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// A failed call: one that may work later is a failure of the machine, others are permanent.
fn api(error: GoogleError) -> ExecutorError {
    match error.retryable() {
        true => machine(error.to_string()),
        false => ExecutorError::Unsupported(format!("Google Batch refused a request: {error}")),
    }
}
