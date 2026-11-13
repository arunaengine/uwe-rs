//! Runs task attempts as Nomad batch jobs: places the files of the task on shared folders or in
//! the staging root, registers the job, polls it, stops a cancelled one, and collects results.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use executor::batch::{Helper, Source};
use executor::binary::Binary;
use executor::capture::{RECORDS, capture};
use executor::process::Mount;
use executor::registry::{self, ImageConfigs};
use executor::remote::RemoteStage;
use executor::wrapper::{Folders, timed, wrapper};
use executor::{Access, Backend, CONTROL, ContainerRun, ExecutorError, Finished, HOME, Job, place};
use task_model::TaskSpec;
use tokio_util::sync::CancellationToken;

use crate::client::{Client, NomadError};
use crate::job::{Artifact, Shape, TASK};
use crate::outcome::{Ended, Seen, finish, machine, seen, unplaceable};
use crate::settings::{Cleanup, Settings};

/// Where Nomad puts the helper in the task folder, which the container sees as
/// `$NOMAD_TASK_DIR`.
const ARTIFACT: &str = "local/uwe";

/// Copies the helper from the task folder of Nomad to `$1`, where the job command runs it, and
/// runs the rest of the words.
const PREPARE: &str = "mkdir -p \"${1%/*}\" && cp \"$NOMAD_TASK_DIR/uwe\" \"$1\" && chmod 755 \
                       \"$1\" || { echo \"uwe: cannot place the helper\" >&2; exit 125; }\n\
                       shift\nexec \"$@\"\n";

/// How many bytes of the end of the log of a failed task go into its error.
const LOG_BYTES: u64 = 4096;

/// The Nomad executor's backend.
#[derive(Debug)]
pub struct NomadBackend {
    pub client: Client,
    pub settings: Settings,
    /// Where each attempt gets its local folder: the folder of the task, which the Nomad
    /// clients see at the same path for shared-path staging, or where its results come back.
    pub scratch: PathBuf,
    /// The staging root, which this machine and the jobs reach; `None` for shared paths.
    pub stage: Option<RemoteStage>,
    /// The binary that uwe uploads for the jobs, when the settings name no helper.
    pub upload: Option<Binary>,
    /// How much longer than its task a job may run: for its image pull and staging.
    pub staging_time: Duration,
    /// The entrypoints and default commands of images, for tasks without a command.
    pub configs: ImageConfigs,
    held: AtomicBool,
}

impl NomadBackend {
    /// A backend whose jobs see the scratch folder at the same path, or with `stage`, get
    /// their files through the staging root and the helper as `settings` say, or else `upload`.
    pub fn new(
        client: Client,
        settings: Settings,
        scratch: PathBuf,
        stage: Option<RemoteStage>,
        upload: Option<Binary>,
    ) -> Result<NomadBackend, String> {
        if stage.is_some() && settings.helper.is_none() && upload.is_none() {
            return Err(
                "Nomad jobs need the uwe helper for object-store staging: set \
                        helper_path, helper_url or helper_file in [executor.nomad]"
                    .into(),
            );
        }
        if stage.as_ref().is_some_and(|stage| stage.credentials) {
            return Err("Nomad staging uses presigned requests; leave out staging_access".into());
        }
        Ok(NomadBackend {
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

impl Backend for NomadBackend {
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
            // Nomad runs the entrypoint of the image around the command of the job.
            let config = self.configs.get(&image.reference).await?;
            spec.arguments = config.cmd.clone();
        }
        if spec.arguments.is_empty() {
            return Err(ExecutorError::NoCommand(image.reference.clone()));
        }
        let local = self.scratch.join(job.name);
        tokio::fs::create_dir_all(&self.scratch).await?;
        let mut staged = None;
        let result = match &self.stage {
            Some(stage) => {
                // Another attempt with this name owns its folder.
                tokio::fs::create_dir(&local).await?;
                let result = self
                    .remote(job, &spec, stage, &local, &mut staged, cancel)
                    .await;
                if let Some((prefix, plan)) = &staged {
                    stage.clean(prefix, plan).await;
                }
                result
            }
            None => {
                let mounts = {
                    let (root, staging) = (local.clone(), job.staging.clone());
                    tokio::task::spawn_blocking(move || place(&root, &staging))
                        .await
                        .map_err(|error| std::io::Error::other(error.to_string()))??
                };
                self.shared(job, &spec, &local, &mounts, cancel).await
            }
        };
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&local).await;
        }
        result
    }
}

impl NomadBackend {
    /// Refuses what a Nomad job cannot do, before anything is placed.
    fn admit(&self, job: Job<'_>, spec: &TaskSpec) -> Result<(), ExecutorError> {
        let refuse = |message: &str| Err(ExecutorError::Unsupported(message.to_owned()));
        if job.placement.offline && self.stage.is_some() {
            return refuse("Nomad jobs need the network for object-store staging");
        }
        if !spec.containers.is_empty() {
            return refuse("the Nomad executor runs only tasks of one container");
        }
        if spec.run_as.is_some() {
            return refuse("the Nomad executor runs a task as the user of uwe or of its image");
        }
        let absolute = (job.staging.inputs.iter().map(|input| &input.path))
            .chain(&job.staging.directories)
            .any(|path| path.starts_with('/'));
        if absolute && self.stage.is_some() {
            return refuse("Nomad object-store staging cannot place inputs at absolute paths");
        }
        Ok(())
    }

    /// Runs an attempt whose folder `root` the Nomad clients see at the same path: the
    /// container runs the wrapper there as the user who owns the folder.
    async fn shared(
        &self,
        job: Job<'_>,
        spec: &TaskSpec,
        root: &Path,
        mounts: &[Mount],
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        let image = spec.image.as_ref().ok_or(ExecutorError::NoImage)?;
        let owner = tokio::fs::metadata(root).await?;
        let access = Access::default();
        let run = ContainerRun {
            name: job.name,
            image,
            root,
            workdir: spec.workdir.as_deref(),
            mounts,
            owner: (owner.uid(), owner.gid()),
            cpus: job.placement.cpus.max(1),
            memory: job.placement.memory_bytes,
            offline: job.placement.offline,
            wrapper_path: None,
            access: &access,
        };
        tokio::fs::write(run.script(), run.wrapper(spec)).await?;
        let time = job.placement.time_seconds;
        let command = match time {
            None => vec!["sh".to_owned(), run.script()],
            Some(_) => {
                let text = format!("{}exit \"$status\"\n", timed("sh \"$1\"", time));
                vec!["sh".into(), "-c".into(), text, "uwe".into(), run.script()]
            }
        };
        // A folder on a volume of the settings needs no bind mount of its own.
        let binds = (run.binds().into_iter())
            .filter(|bind| {
                let source = bind.source.to_string_lossy();
                !self.settings.volumes.iter().any(|volume| {
                    bind.target == source && Path::new(&bind.target).starts_with(&volume.path)
                })
            })
            .collect();
        let id = crate::job::job_id(job.name);
        let shape = Shape {
            id: &id,
            attempt: job.name,
            image: &image.reference,
            command,
            work_dir: run.start(),
            user: Some(format!("{}:{}", owner.uid(), owner.gid())),
            mounts: binds,
            artifact: None,
            placement: job.placement,
        };
        let ended = self.submit(&shape, cancel).await?;
        let finished = finish(&ended, root.to_owned(), time, spec).await;
        let finished = finished.and_then(|mut finished| {
            finished.collect_outputs(spec, job.staging)?;
            Ok(finished)
        });
        self.end(&id, &ended, finished).await
    }

    /// Runs an attempt through the staging root: the helper in the job stages the inputs in,
    /// runs the wrapper and sends the outputs back, which come to `local`.
    async fn remote(
        &self,
        job: Job<'_>,
        spec: &TaskSpec,
        stage: &RemoteStage,
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
        let planned = stage
            .plan_discovery(job, &root, &extra, &[], RECORDS, false, discovery)
            .await?;
        *staged = Some((planned.prefix.clone(), planned.plan.clone()));
        let time = job.placement.time_seconds;
        let fetched = format!("{}.uwe", text(&root));
        let uploaded = match (&self.settings.helper, &self.upload) {
            (None, Some(binary)) => Some((stage.binary(binary).await?, binary)),
            _ => None,
        };
        let mut mounts = Vec::new();
        let (source, artifact) = match (&self.settings.helper, &uploaded) {
            (Some(Helper::Path(path)), _) => {
                let parent = match path.rsplit_once('/') {
                    Some(("", _)) | None => "/",
                    Some((parent, _)) => parent,
                };
                mounts.push(Mount {
                    source: PathBuf::from(parent),
                    target: parent.to_owned(),
                    read_only: true,
                });
                (Source::Path(path), None)
            }
            (Some(Helper::Download { url, sha256 }), _) => {
                let artifact = Artifact {
                    url,
                    sha256,
                    path: ARTIFACT,
                };
                (Source::Path(&fetched), Some(artifact))
            }
            (None, Some((url, binary))) => {
                let artifact = Artifact {
                    url,
                    sha256: binary.digest.hex(),
                    path: ARTIFACT,
                };
                (Source::Path(&fetched), Some(artifact))
            }
            (None, None) => {
                return Err(ExecutorError::Unsupported(
                    "no uwe helper for the jobs".into(),
                ));
            }
        };
        let mut command = executor::batch::command(&text(&root), &planned.location, source, time);
        if artifact.is_some() {
            let mut words = vec!["sh".into(), "-c".into(), PREPARE.into(), "uwe".into()];
            words.push(fetched.clone());
            words.append(&mut command);
            command = words;
        }
        let id = crate::job::job_id(job.name);
        let shape = Shape {
            id: &id,
            attempt: job.name,
            image: &image.reference,
            command,
            work_dir: None,
            user: None,
            mounts,
            artifact,
            placement: job.placement,
        };
        let ended = self.submit(&shape, cancel).await?;
        let collected =
            executor::remote::patiently(|| stage.collect(&planned.prefix, local, RECORDS)).await;
        if let Err(error) = collected {
            // The objects stay, as they are the only copy of the results.
            *staged = None;
            return Err(error);
        }
        let finished = finish(&ended, local.to_owned(), time, spec).await;
        self.end(&id, &ended, finished).await
    }

    /// Registers the job of `shape` and waits for its end.
    async fn submit(
        &self,
        shape: &Shape<'_>,
        cancel: &CancellationToken,
    ) -> Result<Ended, ExecutorError> {
        let body = crate::job::body(shape, &self.settings).map_err(ExecutorError::Unsupported)?;
        if cancel.is_cancelled() || self.held.load(Ordering::Relaxed) {
            return Err(ExecutorError::Cancelled);
        }
        self.client.register(&body).await.map_err(api)?;
        tracing::debug!(
            job = shape.id,
            attempt = shape.attempt,
            "[NOMAD] registered"
        );
        let seconds = shape.placement.time_seconds;
        self.wait(shape.id, seconds, cancel).await
    }

    /// Polls the job until it ended, and stops it when `cancel` fires, when no node takes it,
    /// or when it runs longer than its time and the staging margin.
    async fn wait(
        &self,
        id: &str,
        seconds: Option<u64>,
        cancel: &CancellationToken,
    ) -> Result<Ended, ExecutorError> {
        let registered = Instant::now();
        let mut started: Option<Instant> = None;
        let limit = seconds.map(|seconds| Duration::from_secs(seconds) + self.staging_time);
        loop {
            tokio::select! {
                () = tokio::time::sleep(self.settings.poll) => {}
                () = cancel.cancelled() => {
                    self.stop(id, self.settings.cleanup != Cleanup::Never).await;
                    return Err(ExecutorError::Cancelled);
                }
            }
            let allocations = match self.client.allocations(id).await {
                Ok(allocations) => allocations,
                Err(error) => {
                    self.stop(id, false).await;
                    return Err(api(error));
                }
            };
            match seen(&allocations) {
                Seen::Ended(ended) => {
                    // Nomad may place a failed job again by its reschedule policy, after the
                    // attempt and its staging ended.
                    if ended.failed {
                        self.stop(id, false).await;
                    }
                    return Ok(ended);
                }
                Seen::Running | Seen::Pending => {
                    let started = *started.get_or_insert_with(Instant::now);
                    if limit.is_some_and(|limit| started.elapsed() > limit) {
                        self.stop(id, false).await;
                        return Err(machine(format!(
                            "the job {id} ran longer than the time of its task and the margin \
                             for staging; uwe stopped it"
                        )));
                    }
                }
                Seen::Unplaced => {
                    let evaluations = match self.client.evaluations(id).await {
                        Ok(evaluations) => evaluations,
                        Err(error) => {
                            self.stop(id, false).await;
                            return Err(api(error));
                        }
                    };
                    let problem = unplaceable(&evaluations);
                    if let Some(problem) = problem.filter(|_| self.settings.fail_unplaced) {
                        self.stop(id, true).await;
                        return Err(ExecutorError::Unsupported(format!(
                            "[NOMAD] Job placement failed - no suitable nodes with available \
                             resources: {problem}"
                        )));
                    }
                    if registered.elapsed() > self.settings.placement {
                        self.stop(id, true).await;
                        let minutes = self.settings.placement.as_secs() / 60;
                        return Err(machine(format!(
                            "no Nomad node took the job {id} in {minutes} minutes"
                        )));
                    }
                }
            }
        }
    }

    /// Purges the job as the cleanup setting says, and adds the end of the task's log to a
    /// failure of the machine.
    async fn end(
        &self,
        id: &str,
        ended: &Ended,
        finished: Result<Finished, ExecutorError>,
    ) -> Result<Finished, ExecutorError> {
        let success = matches!(&finished, Ok(done) if done.exit_code == Some(0));
        let purge = match self.settings.cleanup {
            Cleanup::Always => true,
            Cleanup::Never => false,
            Cleanup::OnSuccess => success,
        };
        if purge {
            tracing::debug!("[NOMAD] purgeJob with jobId={id}");
            self.stop(id, true).await;
        }
        match finished {
            Err(ExecutorError::Engine { engine, message }) => {
                let log = self.log(&ended.alloc).await;
                Err(ExecutorError::Engine {
                    engine,
                    message: format!("{message}{log}"),
                })
            }
            finished => finished,
        }
    }

    /// Stops a job; a failure leaves it as it is, so it is a warning.
    async fn stop(&self, id: &str, purge: bool) {
        if let Err(error) = self.client.stop(id, purge).await {
            tracing::warn!("[NOMAD] Unable to stop job {id} -- {error}");
        }
    }

    /// The end of the standard error of the task in `alloc`, as a note for its error; empty
    /// when there is none.
    async fn log(&self, alloc: &str) -> String {
        if alloc.is_empty() {
            return String::new();
        }
        match self.client.log_tail(alloc, TASK, LOG_BYTES).await {
            Ok(text) if !text.trim().is_empty() => format!("\n{}", text.trim_end()),
            Ok(_) => String::new(),
            Err(error) => {
                tracing::debug!(alloc, %error, "cannot read the log of the task");
                String::new()
            }
        }
    }
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// A failed call: one that may work later is a failure of the machine, others are permanent.
fn api(error: NomadError) -> ExecutorError {
    match error.retryable() {
        true => machine(error.to_string()),
        false => ExecutorError::Unsupported(format!("Nomad refused a request: {error}")),
    }
}
