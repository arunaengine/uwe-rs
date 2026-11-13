//! Runs task attempts as Flyte runs: stages through the staging root, creates the run of a
//! raw container, polls it, aborts a cancelled one, and collects what it sent back.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use executor::batch::{Helper, Source};
use executor::binary::Binary;
use executor::capture::{RECORDS, capture};
use executor::registry::{self, ImageConfigs};
use executor::remote::RemoteStage;
use executor::wrapper::{Folders, wrapper};
use executor::{Access, Backend, CONTROL, ContainerRun, ExecutorError, Finished, HOME, Job};
use serde_json::json;
use task_model::TaskSpec;
use tokio_util::sync::CancellationToken;

use crate::client::{Client, FlyteError};
use crate::outcome::{finish, machine};
use crate::run::{ABORT, CREATE, DETAILS, HELPER, LOGS, Phase, Project, ROOT, Seen, Shape};
use crate::settings::Settings;

/// Copies the helper that Flyte downloaded to `$2` (a file, or a folder that holds it) to `$1`,
/// where the job command runs it, and runs the rest of the words.
const PREPARE: &str = "source=\"$2\"\n[ -d \"$source\" ] && for file in \"$source\"/*; do \
                       source=\"$file\"; break; done\nmkdir -p \"${1%/*}\" && cp \"$source\" \"$1\" \
                       && chmod 755 \"$1\" || { echo \"uwe: cannot place the helper that Flyte \
                       downloaded\" >&2; exit 125; }\nshift 2\nexec \"$@\"\n";

/// How many polls in a row may miss a run before it counts as lost.
const MISSES: u32 = 30;

/// How many lines of the end of the log of a failed run go into its error, and how long uwe
/// reads the log stream.
const LOG_LINES: usize = 30;
const LOG_WAIT: Duration = Duration::from_secs(10);

/// The Flyte executor's backend.
#[derive(Debug)]
pub struct FlyteBackend {
    pub client: Client,
    pub project: Project,
    pub settings: Settings,
    /// Where each attempt gets the local folder that its results come back to.
    pub scratch: PathBuf,
    /// The staging root, which this machine, Flyte and the containers reach.
    pub stage: RemoteStage,
    /// The binary that uwe uploads for the runs, when the settings name no helper.
    pub upload: Option<Binary>,
    /// How much longer than its task a run may take: for its image pull and staging.
    pub staging_time: Duration,
    /// The entrypoints and default commands of images, for tasks without a command.
    pub configs: ImageConfigs,
    held: AtomicBool,
}

impl FlyteBackend {
    /// A backend whose runs get the helper as `settings` say, or else `upload` from the
    /// staging root.
    pub fn new(
        client: Client,
        project: Project,
        settings: Settings,
        scratch: PathBuf,
        stage: RemoteStage,
        upload: Option<Binary>,
    ) -> Result<FlyteBackend, String> {
        if settings.helper.is_none() && upload.is_none() {
            return Err(
                "Flyte runs need the uwe helper: set helper_path, helper_url or \
                        helper_file in [executor.flyte]"
                    .into(),
            );
        }
        if stage.credentials {
            return Err("Flyte staging uses presigned requests; leave out staging_access".into());
        }
        Ok(FlyteBackend {
            client,
            project,
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

impl Backend for FlyteBackend {
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
            // Flyte runs the entrypoint of the image around the arguments of the container.
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

/// Refuses what a Flyte run cannot do, before anything is uploaded.
fn admit(job: Job<'_>, spec: &TaskSpec) -> Result<(), ExecutorError> {
    let refuse = |message: &str| Err(ExecutorError::Unsupported(message.to_owned()));
    if job.placement.offline {
        return refuse("Flyte runs need the network for object-store staging");
    }
    if !spec.containers.is_empty() {
        return refuse("the Flyte executor runs only tasks of one container");
    }
    if spec.run_as.is_some() {
        return refuse("Flyte runs a task as the user of its image; it cannot set one");
    }
    let absolute = (job.staging.inputs.iter().map(|input| &input.path))
        .chain(&job.staging.directories)
        .any(|path| path.starts_with('/'));
    if absolute {
        return refuse("Flyte object-store staging cannot place inputs at absolute paths");
    }
    Ok(())
}

impl FlyteBackend {
    /// Stages an attempt, creates its run and waits for its end. The objects of the attempt go
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
        let (fetched, inputs) = (
            format!("{}.uwe", text(&root)),
            format!("{}.in", text(&root)),
        );
        let uploaded = match (&self.settings.helper, &self.upload) {
            (None, Some(binary)) => {
                self.stage.binary(binary).await?;
                let location = content_store::layout::content(&binary.digest);
                Some((self.stage.objects.url(&location), binary))
            }
            _ => None,
        };
        let (source, helper) = match (&self.settings.helper, &uploaded) {
            (Some(Helper::Path(path)), _) => (Source::Path(path), None),
            (Some(Helper::Download { url, sha256 }), _) => {
                let source = Source::Fetched {
                    path: &fetched,
                    sha256,
                    machine: None,
                };
                (source, Some(url.as_str()))
            }
            (None, Some((uri, binary))) => {
                let source = Source::Fetched {
                    path: &fetched,
                    sha256: binary.digest.hex(),
                    machine: Some(binary.machine.uname()),
                };
                (source, Some(uri.as_str()))
            }
            (None, None) => {
                return Err(ExecutorError::Unsupported(
                    "no uwe helper for the runs".into(),
                ));
            }
        };
        let time = job.placement.time_seconds;
        let mut args = executor::batch::command(&text(&root), &planned.location, source, time);
        if helper.is_some() {
            let downloaded = format!("{inputs}/{HELPER}");
            let mut words = vec!["sh".into(), "-c".into(), PREPARE.into(), "uwe".into()];
            words.extend([fetched.clone(), downloaded]);
            words.append(&mut args);
            args = words;
        }
        let moment = (SystemTime::now().duration_since(SystemTime::UNIX_EPOCH))
            .map_or(0, |elapsed| elapsed.as_nanos());
        let name = crate::run::run_name(job.name, moment);
        let shape = Shape {
            name: &name,
            attempt: job.name,
            image: &image.reference,
            args,
            placement: job.placement,
            seconds: time.map(|time| time + self.staging_time.as_secs()),
            queue: (job.placement.queue.as_deref()).or(self.settings.queue.as_deref()),
            helper,
            inputs: &inputs,
        };
        let body = crate::run::body(&shape, &self.project);
        if cancel.is_cancelled() || self.held.load(Ordering::Relaxed) {
            return Err(ExecutorError::Cancelled);
        }
        self.client.call(CREATE, &body).await.map_err(api)?;
        tracing::debug!(run = name, attempt = job.name, "[FLYTE] created the run");
        let seen = self.wait(&name, cancel).await?;
        let collected =
            executor::remote::patiently(|| self.stage.collect(&planned.prefix, local, RECORDS))
                .await;
        if let Err(error) = collected {
            // The objects stay, as they are the only copy of the results.
            *staged = None;
            return Err(error);
        }
        match finish(&seen, local.to_owned(), time, spec).await {
            Err(ExecutorError::Engine { engine, message }) => {
                let log = self.log(&name, seen.attempt).await;
                Err(ExecutorError::Engine {
                    engine,
                    message: format!("{message}{log}"),
                })
            }
            finished => finished,
        }
    }

    /// Polls the run until it ended, and aborts it when `cancel` fires.
    async fn wait(&self, name: &str, cancel: &CancellationToken) -> Result<Seen, ExecutorError> {
        let body = json!({"runId": self.project.run(name)});
        let mut misses = 0;
        loop {
            tokio::select! {
                () = tokio::time::sleep(self.settings.poll) => {}
                () = cancel.cancelled() => {
                    self.abort(name, "uwe cancelled the task").await;
                    return Err(ExecutorError::Cancelled);
                }
            }
            let answer = match self.client.call(DETAILS, &body).await {
                Ok(answer) => answer,
                // A new run may not be known to every replica yet.
                Err(FlyteError::Status { code, .. }) if code == "not_found" => {
                    misses += 1;
                    if misses >= MISSES {
                        return Err(machine(format!("Flyte does not know the run {name}")));
                    }
                    continue;
                }
                Err(error) => {
                    self.abort(name, "uwe lost touch with Flyte").await;
                    return Err(api(error));
                }
            };
            misses = 0;
            let seen = crate::run::seen(&answer);
            if !matches!(seen.phase, Phase::Waiting | Phase::Running) {
                return Ok(seen);
            }
        }
    }

    /// Aborts a run; a failure leaves it running, so it is a warning.
    async fn abort(&self, name: &str, why: &str) {
        let body = json!({"runId": self.project.run(name), "reason": why});
        if let Err(error) = self.client.call(ABORT, &body).await {
            tracing::warn!("[FLYTE] Unable to abort Flyte run {name} -- {error}");
        }
    }

    /// The end of the log of the run's task, as a note for its error; empty when there is
    /// none.
    async fn log(&self, name: &str, attempt: u64) -> String {
        let action = json!({"run": self.project.run(name), "name": ROOT});
        let body = json!({"actionId": action, "attempt": attempt});
        match self.client.stream(LOGS, &body, LOG_WAIT).await {
            Ok(messages) => {
                let text = crate::run::log_text(&messages);
                let lines: Vec<&str> = text.lines().collect();
                let tail = &lines[lines.len().saturating_sub(LOG_LINES)..];
                match tail.is_empty() {
                    true => String::new(),
                    false => format!("\n{}", tail.join("\n")),
                }
            }
            Err(error) => {
                tracing::debug!(run = name, %error, "cannot read the log of the run");
                String::new()
            }
        }
    }
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// A failed call: one that may work later is a failure of the machine, others are permanent.
fn api(error: FlyteError) -> ExecutorError {
    match error.retryable() {
        true => machine(error.to_string()),
        false => ExecutorError::Unsupported(format!("Flyte refused a request: {error}")),
    }
}
