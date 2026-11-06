//! Runs task attempts as AWS Batch jobs: stages through the staging root, submits the job,
//! polls all running jobs together, stops a cancelled one, and collects what it sent back.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use executor::binary::Binary;
use executor::capture::{RECORDS, capture};
use executor::registry::{self, ImageConfigs};
use executor::remote::RemoteStage;
use executor::wrapper::{Folders, wrapper};
use executor::{Access, Backend, CONTROL, ContainerRun, ExecutorError, Finished, HOME, Job};
use serde_json::{Value, json};
use task_model::TaskSpec;
use tokio_util::sync::CancellationToken;

use crate::client::{BatchError, Client};
use crate::definition::{Definitions, Shape, log_group};
use crate::outcome::{Ended, exit_code, finish, log_stream, machine, reason};
use crate::script::Source;
use crate::settings::{Helper, Settings};
use crate::submit::Submission;

/// The reasons of AWS Batch for a job that no compute environment of its queue can run.
const UNSCHEDULABLE: [&str; 2] = [
    "MISCONFIGURATION:JOB_RESOURCE_REQUIREMENT",
    "MISCONFIGURATION:COMPUTE_ENVIRONMENT_MAX_RESOURCE",
];

/// How many polls in a row may miss a job before it counts as lost.
const MISSES: u32 = 30;

/// How many lines of the end of the log of a failed job go into its error.
const LOG_LINES: u32 = 30;

/// The AWS Batch executor's backend.
#[derive(Debug)]
pub struct BatchBackend {
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
    definitions: Definitions,
    watch: Watch,
    held: AtomicBool,
}

/// What the jobs of the backend look like, as one `describejobs` call per poll for up to 100
/// jobs saw them.
#[derive(Debug, Default)]
struct Watch {
    jobs: std::sync::Mutex<HashMap<String, Option<Value>>>,
    asked: tokio::sync::Mutex<Option<Instant>>,
}

impl BatchBackend {
    /// A backend whose jobs get the helper as `settings` say, or else `upload` from the
    /// staging root.
    pub fn new(
        client: Client,
        settings: Settings,
        scratch: PathBuf,
        stage: RemoteStage,
        upload: Option<Binary>,
    ) -> Result<BatchBackend, String> {
        if settings.helper.is_none() && upload.is_none() {
            return Err(
                "AWS Batch jobs need the uwe helper: set helper_path, helper_url or \
                        helper_file in [executor.aws-batch]"
                    .into(),
            );
        }
        if stage.credentials {
            return Err(
                "AWS Batch staging uses presigned requests; leave out staging_access".into(),
            );
        }
        Ok(BatchBackend {
            client,
            settings,
            scratch,
            stage,
            upload,
            staging_time: Duration::from_secs(30 * 60),
            configs: ImageConfigs::default(),
            definitions: Definitions::default(),
            watch: Watch::default(),
            held: AtomicBool::new(false),
        })
    }
}

impl Backend for BatchBackend {
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
        self.admit(job, &spec)?;
        let image = spec.image.as_ref().ok_or(ExecutorError::NoImage)?;
        if spec.arguments.is_empty() {
            // AWS Batch runs the entrypoint of the image around the command of every job.
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

impl BatchBackend {
    /// Refuses what a job of AWS Batch cannot do, before anything is uploaded.
    fn admit(&self, job: Job<'_>, spec: &TaskSpec) -> Result<(), ExecutorError> {
        let refuse = |message: &str| Err(ExecutorError::Unsupported(message.to_owned()));
        if job.placement.offline {
            return refuse("AWS Batch cannot run a task without network");
        }
        if !spec.containers.is_empty() {
            return refuse("AWS Batch runs only tasks of one container");
        }
        if spec.run_as.is_some() {
            return refuse("AWS Batch runs a task as the user of its image; it cannot set one");
        }
        let absolute = (job.staging.inputs.iter().map(|input| &input.path))
            .chain(&job.staging.directories)
            .any(|path| path.starts_with('/'));
        if absolute {
            return refuse("AWS Batch cannot place inputs at absolute paths in the container");
        }
        let image = spec.image.as_ref().ok_or(ExecutorError::NoImage)?;
        if image.reference.starts_with("job-definition://") {
            return refuse(
                "uwe registers its own AWS Batch job definitions; name a container image",
            );
        }
        if job.placement.queue.is_none() && self.settings.queue.is_none() {
            return refuse(
                "AWS Batch needs a job queue: set `queue` in [executor.aws-batch], or the \
                 queue of the task, like Nextflow's `queue` directive",
            );
        }
        Ok(())
    }

    /// Stages an attempt, submits its job and waits for its end. The objects of the attempt go
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
        let download = match &self.upload {
            Some(binary) if self.settings.helper.is_none() => {
                Some(self.stage.binary(binary).await?)
            }
            _ => None,
        };
        let digest = self.upload.as_ref().map(|binary| binary.digest.hex());
        let source = match (&self.settings.helper, &download, &self.upload) {
            (Some(Helper::Path(path)), _, _) => Source::Path(path),
            (Some(Helper::Download { url, sha256 }), _, _) => Source::Download {
                url,
                sha256,
                machine: None,
            },
            (None, Some(url), Some(binary)) => Source::Download {
                url,
                sha256: digest.unwrap_or_default(),
                machine: Some(binary.machine.uname()),
            },
            _ => {
                return Err(ExecutorError::Unsupported(
                    "no uwe helper for the jobs".into(),
                ));
            }
        };
        let time = job.placement.time_seconds;
        let command = crate::script::command(&text(&root), &planned.location, source, time);
        let region = &self.client.region;
        let platform = image.platform.as_deref().unwrap_or_default();
        let shape = Shape {
            image: image.reference.clone(),
            disk_gib: (job.placement.disk_bytes)
                .filter(|_| self.settings.fargate)
                .map(|bytes| bytes.div_ceil(1 << 30).clamp(21, 200)),
            arm: platform.ends_with("arm64") || platform.contains("arm64/"),
        };
        let body = crate::definition::body(&shape, &self.settings, region)
            .map_err(ExecutorError::Unsupported)?;
        let definition = (self.definitions.resolve(&self.client, &body).await).map_err(api)?;
        let queue = (job.placement.queue.as_deref())
            .or(self.settings.queue.as_deref())
            .unwrap_or_default();
        let submission = Submission {
            name: job.name,
            task: job.name,
            queue,
            definition: &definition,
            command,
            placement: job.placement,
            seconds: time.map(|time| time + self.staging_time.as_secs()),
        };
        let body =
            crate::submit::body(&submission, &self.settings).map_err(ExecutorError::Unsupported)?;
        if cancel.is_cancelled() || self.held.load(Ordering::Relaxed) {
            return Err(ExecutorError::Cancelled);
        }
        let answer = self.client.batch("submitjob", &body).await.map_err(api)?;
        let id = (answer["jobId"].as_str())
            .ok_or_else(|| machine("AWS Batch answered the submission without a job id"))?
            .to_owned();
        tracing::debug!(job = id, attempt = job.name, "[AWS BATCH] submitted");
        self.watch.add(&id);
        let ended = self.wait(&id, job.name, cancel).await;
        self.watch.forget(&id);
        let ended = ended?;
        let collected =
            executor::remote::patiently(|| self.stage.collect(&planned.prefix, local, RECORDS))
                .await;
        if let Err(error) = collected {
            // The objects stay, as they are the only copy of the results.
            *staged = None;
            return Err(error);
        }
        let finished = finish(&ended, local.to_owned(), time, spec).await;
        match finished {
            Err(ExecutorError::MissingImage { reason, .. }) => Err(ExecutorError::MissingImage {
                image: image.reference.clone(),
                reason,
            }),
            Err(ExecutorError::Engine { engine, message }) => {
                let log = self.log(&ended, &id).await;
                Err(ExecutorError::Engine {
                    engine,
                    message: format!("{message}{log}"),
                })
            }
            finished => finished,
        }
    }

    /// Polls the job until it ended, and stops it when `cancel` fires.
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
                    self.terminate(id, "uwe cancelled the task").await;
                    return Err(ExecutorError::Cancelled);
                }
            }
            let seen = match self.watch.look(&self.client, id, self.settings.poll).await {
                Ok(seen) => seen,
                Err(error) => {
                    self.terminate(id, "uwe lost touch with AWS Batch").await;
                    return Err(api(error));
                }
            };
            let Some(seen) = seen else {
                misses += 1;
                if misses >= MISSES {
                    return Err(machine(format!(
                        "AWS Batch does not know the job {id} any more"
                    )));
                }
                continue;
            };
            misses = 0;
            let status = seen["status"].as_str().unwrap_or_default();
            let reason = reason(&seen);
            match status {
                "SUCCEEDED" | "FAILED" => {
                    return Ok(Ended {
                        succeeded: status == "SUCCEEDED",
                        exit_code: exit_code(&seen),
                        reason,
                        log_stream: log_stream(&seen),
                    });
                }
                "RUNNING" => {}
                _ if UNSCHEDULABLE.iter().any(|mark| reason.contains(mark)) => {
                    if self.settings.terminate_unschedulable {
                        self.terminate(id, "no compute environment can run the job")
                            .await;
                        return Err(ExecutorError::Unsupported(format!(
                            "AWS Batch cannot schedule the job {id} of {attempt}: {reason}"
                        )));
                    }
                    if !warned {
                        tracing::warn!(
                            job = id,
                            attempt,
                            reason,
                            "AWS Batch cannot schedule the job; it waits"
                        );
                        warned = true;
                    }
                }
                _ => {}
            }
        }
    }

    /// Stops a job; a failure leaves it running, so it is a warning.
    async fn terminate(&self, id: &str, why: &str) {
        let body = json!({"jobId": id, "reason": why});
        if let Err(error) = self.client.batch("terminatejob", &body).await {
            tracing::warn!(job = id, %error, "cannot stop the AWS Batch job");
        }
    }

    /// The end of the log of a job, as a note for its error; empty when there is none.
    async fn log(&self, ended: &Ended, id: &str) -> String {
        let Some(stream) = &ended.log_stream else {
            return String::new();
        };
        let group = log_group(&self.settings);
        match self.client.log_tail(group, stream, LOG_LINES).await {
            Ok(lines) if !lines.is_empty() => format!("\n{}", lines.join("\n")),
            Ok(_) => String::new(),
            Err(error) => {
                tracing::debug!(job = id, %error, "cannot read the log of the job");
                String::new()
            }
        }
    }
}

impl Watch {
    fn add(&self, id: &str) {
        self.lock().insert(id.to_owned(), None);
    }

    fn forget(&self, id: &str) {
        self.lock().remove(id);
    }

    /// The job as AWS Batch described it at most half a poll ago; one call asks about all
    /// running jobs of the backend.
    async fn look(
        &self,
        client: &Client,
        id: &str,
        poll: Duration,
    ) -> Result<Option<Value>, BatchError> {
        let mut asked = self.asked.lock().await;
        let fresh = asked.is_some_and(|at| at.elapsed() < poll / 2);
        let seen = self.lock().get(id).is_some_and(Option::is_some);
        if !fresh || !seen {
            let ids: Vec<String> = self.lock().keys().cloned().collect();
            for chunk in ids.chunks(100) {
                let answer = client
                    .batch("describejobs", &json!({"jobs": chunk}))
                    .await?;
                let jobs = answer["jobs"].as_array().cloned().unwrap_or_default();
                let mut known = self.lock();
                for job in jobs {
                    if let Some(id) = job["jobId"].as_str()
                        && known.contains_key(id)
                    {
                        known.insert(id.to_owned(), Some(job.clone()));
                    }
                }
            }
            *asked = Some(Instant::now());
        }
        Ok(self.lock().get(id).cloned().flatten())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Option<Value>>> {
        self.jobs
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// A failed call: one that may work later is a failure of the machine, others are permanent.
fn api(error: BatchError) -> ExecutorError {
    match error.retryable() {
        true => machine(error.to_string()),
        false => ExecutorError::Unsupported(format!("AWS Batch refused a request: {error}")),
    }
}
