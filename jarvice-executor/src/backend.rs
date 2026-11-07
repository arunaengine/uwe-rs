//! Runs task attempts as JARVICE batch jobs: places the files of the task on the vault or in the
//! staging root, submits the job script, polls the job, ends a cancelled one, and collects results.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use base64::Engine;
use executor::batch::{Helper, Source};
use executor::binary::Binary;
use executor::capture::{RECORDS, capture};
use executor::process::Mount;
use executor::registry::{self, ImageConfigs};
use executor::remote::RemoteStage;
use executor::wrapper::{Folders, MANIFEST, STDERR, TIMED_OUT, quote, timed, wrapper};
use executor::{Access, Backend, CONTROL, ContainerRun, ExecutorError, Finished, HOME, Job, place};
use serde_json::json;
use task_model::{Placement, TaskSpec};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use crate::client::{Client, JarviceError, Submitted, output_lines};
use crate::settings::Settings;

/// The states of a job that ended, as `/jarvice/status` names them.
const ENDED: [&str; 4] = [
    "COMPLETED",
    "COMPLETED WITH ERROR",
    "CANCELED",
    "TERMINATED",
];

/// The state of a job that runs; the time of its task starts here, not in the queue.
const RUNNING: &str = "PROCESSING STARTING";

/// How many status calls may fail in a row before uwe ends the job.
const FAILED_CALLS: u32 = 10;

/// How many bytes of the end of the output of a failed job go into its error.
const OUTPUT_BYTES: usize = 4096;

/// The JARVICE executor's backend.
#[derive(Debug)]
pub struct JarviceBackend {
    pub client: Client,
    pub settings: Settings,
    /// Where each attempt gets its local folder: on the vault, where the jobs see it at the same
    /// path, or where its results come back.
    pub scratch: PathBuf,
    /// The staging root, which this machine and the jobs reach; `None` for the vault.
    pub stage: Option<RemoteStage>,
    /// The binary that uwe uploads for the jobs, when the settings name no helper.
    pub upload: Option<Binary>,
    /// How much longer than its task a job may run: for its image pull and staging.
    pub staging_time: Duration,
    /// The entrypoints and default commands of images, for tasks without a command.
    pub configs: ImageConfigs,
    held: AtomicBool,
}

impl JarviceBackend {
    /// A backend whose jobs see the scratch folder on the vault, or with `stage`, get their
    /// files through the staging root and the helper as `settings` say, or else `upload`.
    pub fn new(
        client: Client,
        settings: Settings,
        scratch: PathBuf,
        stage: Option<RemoteStage>,
        upload: Option<Binary>,
    ) -> Result<JarviceBackend, String> {
        if stage.is_some() && settings.helper.is_none() && upload.is_none() {
            return Err(
                "JARVICE jobs need the uwe helper for object-store staging: set \
                        helper_path, helper_url or helper_file in [executor.jarvice]"
                    .into(),
            );
        }
        if stage.as_ref().is_some_and(|stage| stage.credentials) {
            return Err("JARVICE staging uses presigned requests; leave out staging_access".into());
        }
        if stage.is_none() && settings.vault.is_none() {
            return Err(
                "JARVICE jobs see the files of uwe only on a vault: set vault in \
                        [executor.jarvice], or use object-store staging"
                    .into(),
            );
        }
        if stage.is_none() && !scratch.starts_with(&settings.vault_path) {
            return Err(format!(
                "the scratch folder {} is not on the vault at {}; run uwe in JARVICE with its \
                 cache on the vault, or use object-store staging",
                scratch.display(),
                settings.vault_path
            ));
        }
        Ok(JarviceBackend {
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

impl Backend for JarviceBackend {
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
        let machine = self.machine_type(job.placement)?;
        let image = spec.image.as_ref().ok_or(ExecutorError::NoImage)?;
        if spec.arguments.is_empty() {
            // JARVICE runs the job script in place of the entrypoint and command of the image.
            let config = self.configs.get(&image.reference).await?;
            spec.arguments = config.command(&[]);
        }
        if spec.arguments.is_empty() {
            return Err(ExecutorError::NoCommand(image.reference.clone()));
        }
        let local = self.scratch.join(job.name);
        tokio::fs::create_dir_all(&self.scratch).await?;
        let result = match &self.stage {
            Some(stage) => {
                // Another attempt with this name owns its folder.
                tokio::fs::create_dir(&local).await?;
                let mut staged = None;
                let result = self
                    .remote(job, &spec, &machine, stage, &local, &mut staged, cancel)
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
                self.shared(job, &spec, &machine, &local, &mounts, cancel)
                    .await
            }
        };
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&local).await;
        }
        result
    }
}

impl JarviceBackend {
    /// Refuses what a JARVICE job cannot do, before anything is placed.
    fn admit(&self, job: Job<'_>, spec: &TaskSpec) -> Result<(), ExecutorError> {
        let refuse = |message: &str| Err(ExecutorError::Unsupported(message.to_owned()));
        if job.placement.offline && self.stage.is_some() {
            return refuse("JARVICE jobs need the network for object-store staging");
        }
        if !spec.containers.is_empty() {
            return refuse("the JARVICE executor runs only tasks of one container");
        }
        if spec.run_as.is_some() {
            return refuse("the JARVICE executor runs a task as the user of its job");
        }
        let absolute = (job.staging.inputs.iter().map(|input| &input.path))
            .chain(&job.staging.directories)
            .any(|path| path.starts_with('/'));
        if absolute && self.stage.is_some() {
            return refuse("JARVICE object-store staging cannot place inputs at absolute paths");
        }
        Ok(())
    }

    /// The machine type of a task: its `machineType` option, its queue, or that of the
    /// settings.
    fn machine_type(&self, placement: &Placement) -> Result<String, ExecutorError> {
        let option = placement.options.get("machineType");
        (option.and_then(serde_json::Value::as_str))
            .or(placement.queue.as_deref())
            .or(self.settings.machine_type.as_deref())
            .map(str::to_owned)
            .ok_or_else(|| {
                ExecutorError::Unsupported(
                    "JARVICE jobs need a machine type: set machine_type in [executor.jarvice], \
                     or a queue or machineType for the task"
                        .into(),
                )
            })
    }

    /// Runs an attempt whose folder `root` the job sees on the vault at the same path: the
    /// container runs the wrapper there.
    async fn shared(
        &self,
        job: Job<'_>,
        spec: &TaskSpec,
        machine: &str,
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
        // JARVICE mounts only the vault, so the job must see every folder there at its path.
        let vault = Path::new(&self.settings.vault_path);
        let foreign = (run.binds().into_iter())
            .find(|bind| bind.source != Path::new(&bind.target) || !bind.source.starts_with(vault));
        if let Some(bind) = foreign {
            return Err(ExecutorError::Unsupported(format!(
                "JARVICE jobs see only the vault at {}, not {} at {}",
                vault.display(),
                bind.source.display(),
                bind.target
            )));
        }
        tokio::fs::write(run.script(), run.wrapper(spec)).await?;
        let time = job.placement.time_seconds;
        let start = run.start().unwrap_or_else(|| text(root));
        let command = format!("sh {}", quote(&run.script()));
        let script = format!(
            "cd {} || exit 125\n{}exit \"$status\"\n",
            quote(&start),
            timed(&command, time)
        );
        let reference = &image.reference;
        let (submitted, status) = (self.submit(job, reference, machine, &script, cancel)).await?;
        let finished = self.finish(&submitted, &status, root.to_owned(), time, spec);
        let mut finished = finished.await?;
        finished.collect_outputs(spec, job.staging)?;
        Ok(finished)
    }

    /// Runs an attempt through the staging root: the helper in the job stages the inputs in,
    /// runs the wrapper and sends the outputs back, which come to `local`.
    #[expect(clippy::too_many_arguments, reason = "the parts of one attempt")]
    async fn remote(
        &self,
        job: Job<'_>,
        spec: &TaskSpec,
        machine: &str,
        stage: &RemoteStage,
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
        let planned = stage
            .plan_discovery(job, &root, &extra, &[], RECORDS, false, discovery)
            .await?;
        *staged = Some((planned.prefix.clone(), planned.plan.clone()));
        let download = match (&self.settings.helper, &self.upload) {
            (None, Some(binary)) => Some(stage.binary(binary).await?),
            _ => None,
        };
        let digest = (self.upload.as_ref()).map_or("", |binary| binary.digest.hex());
        let source = match (&self.settings.helper, &download, &self.upload) {
            (Some(Helper::Path(path)), _, _) => Source::Path(path),
            (Some(Helper::Download { url, sha256 }), _, _) => Source::Download {
                url,
                sha256,
                machine: None,
            },
            (None, Some(url), Some(binary)) => Source::Download {
                url,
                sha256: digest,
                machine: Some(binary.machine.uname()),
            },
            _ => {
                return Err(ExecutorError::Unsupported(
                    "no uwe helper for the jobs".into(),
                ));
            }
        };
        let time = job.placement.time_seconds;
        let words = executor::batch::command(&text(&root), &planned.location, source, time);
        let words: Vec<String> = words.iter().map(|word| quote(word)).collect();
        let script = format!("{}\n", words.join(" "));
        let reference = &image.reference;
        let (submitted, status) = (self.submit(job, reference, machine, &script, cancel)).await?;
        let collected =
            executor::remote::patiently(|| stage.collect(&planned.prefix, local, RECORDS)).await;
        if let Err(error) = collected {
            // The objects stay, as they are the only copy of the results.
            *staged = None;
            return Err(error);
        }
        self.finish(&submitted, &status, local.to_owned(), time, spec)
            .await
    }

    /// Submits the job script of an attempt and waits for the end of its job, whose state it
    /// gives.
    async fn submit(
        &self,
        job: Job<'_>,
        image: &str,
        machine: &str,
        script: &str,
        cancel: &CancellationToken,
    ) -> Result<(Submitted, String), ExecutorError> {
        let encoded = base64::engine::general_purpose::STANDARD.encode(script);
        let mut body = json!({
            "machine": {"type": machine, "nodes": 1},
            "job_label": format!("uwe-{}", job.name),
            "container": {"image": image_name(image), "jobscript": encoded},
        });
        if let Some(vault) = &self.settings.vault {
            body["vault"] = json!({
                "name": vault,
                "readonly": self.settings.vault_readonly,
                "force": self.settings.vault_force,
            });
        }
        if cancel.is_cancelled() || self.held.load(Ordering::Relaxed) {
            return Err(ExecutorError::Cancelled);
        }
        // A submission is not tried again, as a job whose answer is lost would run unwatched.
        let submitted = self.client.submit(&body).await.map_err(api)?;
        tracing::debug!(
            job = submitted.number,
            attempt = job.name,
            "JARVICE job submitted"
        );
        let status = self
            .wait(&submitted, job.placement.time_seconds, cancel)
            .await?;
        Ok((submitted, status))
    }

    /// Polls the job until it ended, and ends it when `cancel` fires, when JARVICE fails to
    /// answer, or when it runs longer than its time and the staging margin.
    async fn wait(
        &self,
        job: &Submitted,
        seconds: Option<u64>,
        cancel: &CancellationToken,
    ) -> Result<String, ExecutorError> {
        let limit = seconds.map(|seconds| Duration::from_secs(seconds) + self.staging_time);
        let (mut started, mut failures): (Option<Instant>, u32) = (None, 0);
        loop {
            tokio::select! {
                () = tokio::time::sleep(self.settings.poll) => {}
                () = cancel.cancelled() => {
                    self.stop(job).await;
                    return Err(ExecutorError::Cancelled);
                }
            }
            match self.client.status(job).await {
                Ok(Some(status)) if ENDED.contains(&status.as_str()) => return Ok(status),
                Ok(status) => {
                    failures = 0;
                    if status.as_deref() == Some(RUNNING) {
                        started.get_or_insert_with(Instant::now);
                    }
                }
                Err(error) => {
                    failures += 1;
                    tracing::debug!(job = job.number, %error, "cannot read the JARVICE job");
                    if failures >= FAILED_CALLS {
                        self.stop(job).await;
                        return Err(api(error));
                    }
                }
            }
            let over = limit
                .zip(started)
                .is_some_and(|(limit, at)| at.elapsed() > limit);
            if over {
                self.stop(job).await;
                return Err(machine(format!(
                    "the JARVICE job {} ran longer than the time of its task and the margin \
                     for staging; uwe ended it",
                    job.number
                )));
            }
        }
    }

    /// Ends a job: a shutdown while it runs, else a termination; a failure leaves it as it is,
    /// so it is a warning.
    async fn stop(&self, job: &Submitted) {
        if self.client.shutdown(job).await.is_ok() {
            return;
        }
        if let Err(error) = self.client.terminate(job).await {
            tracing::warn!(job = job.number, %error, "cannot end the JARVICE job");
        }
    }

    /// What the task left in `root`, from the exit code that its wrapper recorded; a job that
    /// ended without one could not run the task, which the end of its output tells.
    async fn finish(
        &self,
        job: &Submitted,
        status: &str,
        root: PathBuf,
        time: Option<u64>,
        spec: &TaskSpec,
    ) -> Result<Finished, ExecutorError> {
        let control = root.join(CONTROL);
        let exit_code = match executor::containers::recorded(spec, &control) {
            Some(TIMED_OUT) if time.is_some() => {
                let seconds = time.unwrap_or_default();
                let note = format!("uwe: the task ran longer than its time of {seconds} seconds\n");
                let mut file = (tokio::fs::OpenOptions::new().create(true).append(true))
                    .open(control.join(STDERR))
                    .await?;
                file.write_all(note.as_bytes()).await?;
                // A file of Tokio writes in the background until it is flushed.
                file.flush().await?;
                143
            }
            Some(code) => code,
            None => {
                let output = self.output(job).await;
                return Err(machine(format!(
                    "the JARVICE job {} ended as {status} without the exit code of the task{output}",
                    job.number
                )));
            }
        };
        let files = match exit_code {
            0 => {
                let manifest = tokio::fs::read_to_string(control.join(MANIFEST)).await;
                executor::manifest::parse(&manifest.unwrap_or_default())?
            }
            _ => Vec::new(),
        };
        Ok(Finished {
            address: None,
            exit_code: Some(exit_code),
            files,
            usage: executor::Usage::of(&root),
            root,
        })
    }

    /// The end of the output of a job after the header of the platform, as a note for its
    /// error; empty when there is none.
    async fn output(&self, job: &Submitted) -> String {
        match self.client.output(job).await {
            Ok(text) => {
                let lines = output_lines(&text).trim_end();
                let start = lines.len().saturating_sub(OUTPUT_BYTES);
                let start = (start..=lines.len())
                    .find(|index| lines.is_char_boundary(*index))
                    .unwrap_or(lines.len());
                match &lines[start..] {
                    "" => String::new(),
                    tail => format!("\n{tail}"),
                }
            }
            Err(error) => {
                tracing::debug!(job = job.number, %error, "cannot read the output of the job");
                String::new()
            }
        }
    }
}

/// The image as JARVICE takes it: one of Docker Hub without a namespace gets `library/`.
fn image_name(reference: &str) -> String {
    match reference.contains('/') {
        true => reference.to_owned(),
        false => format!("library/{reference}"),
    }
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// A failed call: one that may work later is a failure of the machine, others are permanent.
fn api(error: JarviceError) -> ExecutorError {
    let retryable = match &error {
        JarviceError::Request { .. } => true,
        JarviceError::Status { status, .. } => matches!(status, 408 | 429 | 500..),
    };
    match retryable {
        true => machine(error.to_string()),
        false => ExecutorError::Unsupported(format!("JARVICE refused a request: {error}")),
    }
}

fn machine(message: impl Into<String>) -> ExecutorError {
    ExecutorError::Engine {
        engine: "JARVICE".into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_hub_images() {
        assert_eq!(image_name("ubuntu:22.04"), "library/ubuntu:22.04");
        assert_eq!(
            image_name("quay.io/biocontainers/samtools:1.21"),
            "quay.io/biocontainers/samtools:1.21"
        );
        assert_eq!(
            image_name("library/ubuntu@sha256:ab"),
            "library/ubuntu@sha256:ab"
        );
    }
}
