//! Runs task attempts as Azure Batch tasks: finds or creates their pool and the job of the run
//! on it, stages through the staging root, polls the task, and ends the jobs at the end of the run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use executor::batch::{Helper, Source};
use executor::binary::Binary;
use executor::capture::{RECORDS, capture};
use executor::registry::{self, ImageConfigs};
use executor::remote::RemoteStage;
use executor::wrapper::{Folders, wrapper};
use executor::{Access, Backend, CONTROL, ContainerRun, ExecutorError, Finished, HOME, Job};
use task_model::TaskSpec;
use tokio_util::sync::CancellationToken;

use crate::client::Client;
use crate::jobs::Jobs;
use crate::outcome::{Ended, api, finish, machine};
use crate::pool::{Health, Pool};
use crate::settings::Settings;
use crate::task::{Download, Shape};

/// Where the plan and the helper land in the working folder of a task.
const PLAN_FILE: &str = "uwe-plan.json";
const HELPER_FILE: &str = "uwe";

/// How many polls in a row may miss a task before it counts as lost.
const MISSES: u32 = 30;

/// How many lines of the end of the output of a failed task go into its error, and how many
/// bytes of each output file uwe reads for them.
const LOG_LINES: usize = 30;
const LOG_BYTES: u64 = 16 * 1024;

/// The Azure Batch executor's backend.
#[derive(Debug)]
pub struct AzureBackend {
    pub client: Client,
    pub settings: Settings,
    /// Where each attempt gets the local folder that its results come back to.
    pub scratch: PathBuf,
    /// The staging root, which this machine and the tasks reach.
    pub stage: RemoteStage,
    /// The binary that uwe uploads for the tasks, when the settings name no helper.
    pub upload: Option<Binary>,
    /// How much longer than its task a Batch task may run: for its image pull and staging.
    pub staging_time: Duration,
    /// How often uwe looks at the pool of a task that waits for a node.
    pub pool_check: Duration,
    /// The entrypoints and default commands of images, for tasks without a command.
    pub configs: ImageConfigs,
    jobs: Jobs,
    held: AtomicBool,
}

impl AzureBackend {
    /// A backend whose tasks get the helper as `settings` say, or else `upload` from the
    /// staging root.
    pub fn new(
        client: Client,
        settings: Settings,
        scratch: PathBuf,
        stage: RemoteStage,
        upload: Option<Binary>,
    ) -> Result<AzureBackend, String> {
        if settings.helper.is_none() && upload.is_none() {
            return Err(
                "Azure Batch tasks need the uwe helper: set helper_path, helper_url or \
                        helper_file in [executor.azure-batch]"
                    .into(),
            );
        }
        if stage.credentials {
            return Err(
                "Azure Batch staging uses presigned requests; leave out staging_access".into(),
            );
        }
        Ok(AzureBackend {
            client,
            settings,
            scratch,
            stage,
            upload,
            staging_time: Duration::from_secs(30 * 60),
            pool_check: Duration::from_secs(60),
            configs: ImageConfigs::default(),
            jobs: Jobs::default(),
            held: AtomicBool::new(false),
        })
    }
}

impl Backend for AzureBackend {
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
            // The container keeps the entrypoint of its image around the command line.
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

    /// Ends the jobs of the run once their tasks ended, deletes them when asked, and deletes
    /// the pools that uwe created when asked.
    async fn release(&self) {
        self.jobs.release(&self.client, &self.settings).await;
    }
}

/// Refuses what a task of Azure Batch cannot do, before anything is uploaded.
fn admit(job: Job<'_>, spec: &TaskSpec) -> Result<(), ExecutorError> {
    let refuse = |message: &str| Err(ExecutorError::Unsupported(message.to_owned()));
    if job.placement.offline {
        return refuse("Azure Batch cannot run a task without network, as it stages its files");
    }
    if !spec.containers.is_empty() {
        return refuse("Azure Batch runs only tasks of one container");
    }
    if spec.run_as.is_some() {
        return refuse("Azure Batch runs a task as a user of its pool; set run_as of the pool");
    }
    let absolute = (job.staging.inputs.iter().map(|input| &input.path))
        .chain(&job.staging.directories)
        .any(|path| path.starts_with('/'));
    if absolute {
        return refuse("Azure Batch cannot place inputs at absolute paths in the container");
    }
    Ok(())
}

impl AzureBackend {
    /// Stages an attempt, adds its task and waits for its end. The objects of the attempt go
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
        let (client, settings) = (&self.client, &self.settings);
        let pool = self
            .jobs
            .pool(client, settings, job.name, job.placement)
            .await?;
        let batch_job = self.jobs.job(client, settings, &pool.id).await?;
        let root = Path::new(&self.settings.task_folder).join(job.name);
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
        let mut downloads = vec![Download {
            url: planned.location.clone(),
            path: PLAN_FILE,
            executable: false,
        }];
        let mut volumes = Vec::new();
        let fetched = format!("./{HELPER_FILE}");
        let uploaded = match (&self.settings.helper, &self.upload) {
            (None, Some(binary)) => Some((self.stage.binary(binary).await?, binary)),
            _ => None,
        };
        let digest = self.upload.as_ref().map(|binary| binary.digest.hex());
        let source = match (&self.settings.helper, &uploaded) {
            (Some(Helper::Path(path)), _) => {
                let parent = match path.rsplit_once('/') {
                    Some(("", _)) | None => "/",
                    Some((parent, _)) => parent,
                };
                volumes.push(format!("{parent}:{parent}:ro"));
                Source::Path(path)
            }
            (Some(Helper::Download { url, sha256 }), _) => {
                downloads.push(helper_file(url.clone()));
                Source::Fetched {
                    path: &fetched,
                    sha256,
                    machine: None,
                }
            }
            (None, Some((url, binary))) => {
                downloads.push(helper_file(url.clone()));
                Source::Fetched {
                    path: &fetched,
                    sha256: digest.unwrap_or_default(),
                    machine: Some(binary.machine.uname()),
                }
            }
            (None, None) => {
                return Err(ExecutorError::Unsupported(
                    "no uwe helper for the tasks".into(),
                ));
            }
        };
        let time = job.placement.time_seconds;
        let task_id = crate::task::task_id(job.name);
        let shape = Shape {
            id: &task_id,
            image: &image.reference,
            command: executor::batch::command(&text(&root), PLAN_FILE, source, time),
            downloads,
            volumes,
            placement: job.placement,
            slots: crate::pool::slots(job.placement, &pool),
            seconds: time.map(|time| time + self.staging_time.as_secs()),
            run_as: pool.run_as.as_deref(),
        };
        let body = crate::task::body(&shape).map_err(ExecutorError::Unsupported)?;
        if cancel.is_cancelled() || self.held.load(Ordering::Relaxed) {
            return Err(ExecutorError::Cancelled);
        }
        let added = self.client.create_task(&batch_job, &body).await;
        added.map_err(api)?;
        tracing::debug!(
            job = batch_job,
            task = task_id,
            "[AZURE BATCH] added the task"
        );
        let ended = self.wait(&batch_job, &task_id, &pool, cancel).await?;
        let collected =
            executor::remote::patiently(|| self.stage.collect(&planned.prefix, local, RECORDS))
                .await;
        if let Err(error) = collected {
            // The objects stay, as they are the only copy of the results.
            *staged = None;
            self.clean(&batch_job, &task_id, false).await;
            return Err(error);
        }
        let finished = match finish(&ended, local.to_owned(), time, spec).await {
            Err(ExecutorError::MissingImage { reason, .. }) => Err(ExecutorError::MissingImage {
                image: image.reference.clone(),
                reason,
            }),
            Err(ExecutorError::Engine { engine, message }) => {
                let log = self.log(&batch_job, &task_id).await;
                Err(ExecutorError::Engine {
                    engine,
                    message: format!("{message}{log}"),
                })
            }
            finished => finished,
        };
        let succeeded = finished
            .as_ref()
            .is_ok_and(|done| done.exit_code == Some(0));
        self.clean(&batch_job, &task_id, succeeded).await;
        finished
    }

    /// Polls the task until it completed, looks at its pool while it waits for a node, and
    /// stops it when `cancel` fires.
    async fn wait(
        &self,
        job: &str,
        task: &str,
        pool: &Pool,
        cancel: &CancellationToken,
    ) -> Result<Ended, ExecutorError> {
        let (mut misses, mut warned, mut checked) = (0, false, Instant::now());
        loop {
            tokio::select! {
                () = tokio::time::sleep(self.settings.poll) => {}
                () = cancel.cancelled() => {
                    self.terminate(job, task).await;
                    self.clean(job, task, false).await;
                    return Err(ExecutorError::Cancelled);
                }
            }
            let seen = match self.client.task(job, task).await {
                Ok(seen) => seen,
                Err(error) => {
                    self.terminate(job, task).await;
                    return Err(api(error));
                }
            };
            let Some(seen) = seen else {
                misses += 1;
                if misses >= MISSES {
                    return Err(machine(format!(
                        "Azure Batch does not know the task {task} of the job {job} any more"
                    )));
                }
                continue;
            };
            misses = 0;
            if let Some(ended) = crate::outcome::ended(&seen) {
                return Ok(ended);
            }
            if seen["state"].as_str() != Some("active") || checked.elapsed() < self.pool_check {
                continue;
            }
            checked = Instant::now();
            let Ok(Some(found)) = self.client.pool(&pool.id).await else {
                continue;
            };
            match crate::pool::health(&found, pool) {
                Health::Fine => {}
                Health::Waiting(message) if !warned => {
                    tracing::warn!("{message}");
                    warned = true;
                }
                Health::Waiting(_) => {}
                Health::Broken(message) => {
                    self.terminate(job, task).await;
                    return Err(ExecutorError::Unsupported(message));
                }
            }
        }
    }

    /// Stops a task; a failure leaves it running, so it is a warning.
    async fn terminate(&self, job: &str, task: &str) {
        if let Err(error) = self.client.terminate(job, task).await {
            tracing::warn!(job, task, %error, "cannot stop the Azure Batch task");
        }
    }

    /// Deletes a task that ended, as the settings say: all, none, or the successful ones.
    async fn clean(&self, job: &str, task: &str, succeeded: bool) {
        if !self.settings.delete_tasks.unwrap_or(succeeded) {
            return;
        }
        if let Err(error) = self
            .client
            .delete(&format!("/jobs/{job}/tasks/{task}"))
            .await
        {
            tracing::warn!(
                "Unable to cleanup batch task: {job}/{task} -- see the log file for details"
            );
            tracing::debug!(job, task, %error, "[AZURE BATCH] cannot delete the task");
        }
    }

    /// The end of the standard output and error of a task, as a note for its error; empty when
    /// it has none.
    async fn log(&self, job: &str, task: &str) -> String {
        let mut note = String::new();
        for name in ["stdout.txt", "stderr.txt"] {
            match self.client.file_tail(job, task, name, LOG_BYTES).await {
                Ok(text) if !text.trim().is_empty() => {
                    let lines: Vec<&str> = text.lines().collect();
                    let tail = &lines[lines.len().saturating_sub(LOG_LINES)..];
                    note.push_str(&format!("\n{name}:\n{}", tail.join("\n")));
                }
                Ok(_) => {}
                Err(error) => tracing::debug!(job, task, %error, "cannot read {name} of the task"),
            }
        }
        note
    }
}

/// The helper as a resource file that may run.
fn helper_file(url: String) -> Download {
    Download {
        url,
        path: HELPER_FILE,
        executable: true,
    }
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
