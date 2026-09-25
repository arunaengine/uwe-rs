//! Runs one container through the native jobs contract and immutable bundle transfer.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fs::{File, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use executor::poll::{CANCEL_GRACE, Poll, REQUEST_GRACE, WaitError, until_stopped};
use executor::registry::{self, ImageConfigs};
use executor::{Backend, CancellationToken, ExecutorError, Finished, Job};
use serde::{Deserialize, Serialize};
use task_model::Digest;

use crate::bundle;
use crate::client::{Client, Input, InputMode, JobKind, LogicalState, Output, Status, Submit};
use crate::objects::{MAX_BUNDLE, Objects, collect};

const READ_GRACE: Duration = Duration::from_secs(120);

pub struct ArunaBackend {
    pub client: Client,
    pub scratch: PathBuf,
    /// An authenticated caller's stable replay namespace, distinct for forced new work.
    pub scope: String,
    pub group_id: String,
    pub output_bucket: String,
    pub objects: Arc<dyn Objects>,
    pub configs: ImageConfigs,
}

#[derive(Serialize, Deserialize)]
struct Journal {
    server: String,
    request: Submit,
    id: Option<String>,
    cancel_pending: bool,
    terminal: Option<LogicalState>,
}

impl Backend for ArunaBackend {
    async fn pin(&self, image: &str) -> Result<Option<String>, ExecutorError> {
        registry::pin(image).await
    }

    async fn run(
        &self,
        job: Job<'_>,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        if cancel.is_cancelled() {
            return Err(ExecutorError::Cancelled);
        }
        if self.scope.is_empty() || self.group_id.is_empty() || self.output_bucket.is_empty() {
            return Err(ExecutorError::Unsupported(
                "Aruna requires a caller scope, group and output bucket".into(),
            ));
        }
        if job.placement.offline
            || !job.spec.containers.is_empty()
            || job.spec.run_as.is_some()
            || job.placement.accelerators.iter().any(|gpu| gpu.count > 0)
            || job.placement.disk_bytes.is_some()
            || job.placement.preemptible
            || job.placement.queue.is_some()
            || !job.placement.parameters.is_empty()
        {
            return Err(ExecutorError::Unsupported("Aruna jobs cannot express this task's network, containers, user or placement settings".into()));
        }
        if job.name.is_empty()
            || Path::new(job.name).components().count() != 1
            || job.name == "."
            || job.name == ".."
            || job.name.contains('/')
        {
            return Err(std::io::Error::other("invalid Aruna attempt name").into());
        }
        let cache_key = job
            .spec
            .cache_key()
            .map_err(|error| ExecutorError::Unsupported(error.to_string()))?;
        let identity = serde_json::to_vec(&(
            self.client.server(),
            &self.scope,
            &self.group_id,
            &self.output_bucket,
            cache_key,
        ))
        .map_err(|error| std::io::Error::other(error.to_string()))?;
        let key = format!("iunctus-{}", Digest::sha256(&identity).hex());
        let record = self.scratch.join("journals").join(&key);
        std::fs::create_dir_all(&record)?;
        let lock = File::options()
            .write(true)
            .create(true)
            .truncate(false)
            .open(record.join("lock"))?;
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => tokio::select! {
                    () = tokio::time::sleep(Duration::from_millis(25)) => {},
                    () = cancel.cancelled() => return Err(ExecutorError::Cancelled),
                },
                Err(TryLockError::Error(error)) => return Err(error.into()),
            }
        }
        let mut spec = job.spec.clone();
        let image = spec.image.as_mut().ok_or(ExecutorError::NoImage)?;
        if image.platform.is_some() {
            let resolved = self
                .configs
                .resolved_for(&image.reference, image.platform.as_deref())
                .await?;
            image.reference = resolved.reference;
            if !spec.replaces_entrypoint || spec.arguments.is_empty() {
                spec.arguments = resolved.config.command(&spec.arguments);
            }
        } else if !spec.replaces_entrypoint || spec.arguments.is_empty() {
            spec.arguments = self
                .configs
                .get(&image.reference)
                .await?
                .command(&spec.arguments);
        }
        if spec.arguments.is_empty() {
            return Err(ExecutorError::NoCommand(image.reference.clone()));
        }
        let job = Job { spec: &spec, ..job };
        let path = record.join("request.json");
        let mut journal: Journal = if path.exists() {
            if path.metadata()?.len() > 1024 * 1024 {
                return Err(unresolved("oversized submission journal"));
            }
            let journal: Journal = serde_json::from_slice(&std::fs::read(&path)?)
                .map_err(|_| unresolved("invalid submission journal"))?;
            let expected = self.request(job, &key, journal.request.inputs.clone())?;
            if journal.server != self.client.server()
                || serde_json::to_value(expected).ok()
                    != serde_json::to_value(&journal.request).ok()
            {
                return Err(unresolved(
                    "the submission plan changed for the same cache key; use a new caller scope for new work",
                ));
            }
            journal
        } else {
            let (directory, staging, spec, placement, name) = (
                record.clone(),
                job.staging.clone(),
                spec.clone(),
                job.placement.clone(),
                job.name.to_owned(),
            );
            let input = tokio::task::spawn_blocking(move || {
                bundle::prepare(
                    Job {
                        name: &name,
                        spec: &spec,
                        staging: &staging,
                        placement: &placement,
                    },
                    &directory,
                )
            })
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))??;
            let object = self
                .objects
                .write(&self.group_id, &format!("{key}/input.tar"), &input)
                .await?;
            if object.bucket.is_empty() || object.key.is_empty() || object.version_id.is_empty() {
                return Err(unresolved("input upload returned no immutable reference"));
            }
            let inputs = vec![Input {
                bucket: object.bucket,
                key: object.key,
                version_id: object.version_id,
                dest_key: format!("{key}/input.tar"),
                container_path: bundle::INPUT.into(),
                mode: InputMode::ExactReference,
            }];
            let journal = Journal {
                server: self.client.server().into(),
                request: self.request(job, &key, inputs)?,
                id: None,
                cancel_pending: false,
                terminal: None,
            };
            save(&record, &journal)?;
            journal
        };
        if cancel.is_cancelled() && journal.id.is_none() {
            return Err(ExecutorError::Cancelled);
        }
        // Await the bounded submit even during cancellation: dropping it loses accepted identity.
        let submitted = self.client.submit(&journal.request).await.map_err(|_| {
            unresolved("submission may have been accepted; replay the retained journal")
        })?;
        if journal
            .id
            .as_ref()
            .is_some_and(|id| *id != submitted.canonical_job_id)
        {
            return Err(unresolved("idempotent replay changed its canonical job"));
        }
        journal.id = Some(submitted.canonical_job_id);
        save(&record, &journal)?;
        let status = self
            .wait(&record, &mut journal, job.placement.time_seconds, cancel)
            .await?;
        match terminal(&status) {
            Some(LogicalState::Cancelled) => return Err(ExecutorError::Cancelled),
            Some(LogicalState::Failed) => {
                return Err(machine(
                    status
                        .error
                        .map(|error| format!("{}: {}", error.kind, error.message))
                        .unwrap_or_else(|| "job failed without a reason".into()),
                ));
            }
            Some(LogicalState::Succeeded) => {}
            _ => return Err(unresolved("job ended without a complete family verdict")),
        }
        let family = status
            .family
            .as_ref()
            .expect("terminal verdict requires a family");
        let outputs: Vec<_> = family
            .outputs
            .iter()
            .filter(|object| {
                object.container_path == bundle::RESULT
                    && Some(&object.execution_id) == family.canonical_execution_id.as_ref()
            })
            .collect();
        let object = match outputs.as_slice() {
            [object]
                if object.bucket == self.output_bucket
                    && object.key == journal.request.outputs[0].dest_key
                    && !object.version_id.is_empty()
                    && object.size <= MAX_BUNDLE =>
            {
                *object
            }
            _ => {
                return Err(unresolved(
                    "canonical execution has no unique immutable result bundle",
                ));
            }
        };
        let bytes = self.objects.read(object, MAX_BUNDLE).await?;
        if bytes.len() as u64 != object.size || bytes.len() as u64 > MAX_BUNDLE {
            return Err(unresolved(
                "result bundle size disagrees with its immutable object",
            ));
        }
        let root = self.scratch.join("attempts").join(job.name);
        std::fs::create_dir_all(root.parent().expect("attempt parent"))?;
        std::fs::create_dir(&root)?;
        let directory = root.clone();
        let result = tokio::task::spawn_blocking(move || collect(&bytes, &directory))
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&root);
        }
        result
    }
}

impl ArunaBackend {
    fn request(
        &self,
        job: Job<'_>,
        key: &str,
        inputs: Vec<Input>,
    ) -> Result<Submit, ExecutorError> {
        let walltime = job
            .placement
            .time_seconds
            .map(|seconds| {
                seconds.checked_mul(1000).ok_or_else(|| {
                    ExecutorError::Unsupported("Aruna walltime overflows milliseconds".into())
                })
            })
            .transpose()?;
        Ok(Submit {
            group_id: self.group_id.clone(), image: job.spec.image.as_ref().ok_or(ExecutorError::NoImage)?.reference.clone(),
            name: None, entrypoint: Some(vec!["sh".into(), "-c".into()]),
            command: vec!["mkdir -p /iunctus && tar -xf /iunctus/input.tar -C /iunctus && exec sh /iunctus/start.sh".into()],
            env: Default::default(), workdir: Some("/".into()), cpu_cores: Some(job.placement.cpus.max(1)),
            ram_bytes: job.placement.memory_bytes, max_walltime_ms: walltime,
            executor_constraint: job.placement.executor.clone(), tags: Default::default(), inputs,
            outputs: vec![Output { bucket: self.output_bucket.clone(), dest_key: format!("{key}/result.tar"), container_path: bundle::RESULT.into() }],
            idempotency_key: key.into(),
        })
    }

    async fn wait(
        &self,
        record: &Path,
        journal: &mut Journal,
        seconds: Option<u64>,
        cancel: &CancellationToken,
    ) -> Result<Status, ExecutorError> {
        if journal.cancel_pending || cancel.is_cancelled() {
            self.stop(record, journal).await?;
            return Err(ExecutorError::Cancelled);
        }
        let id = journal.id.as_ref().expect("submitted identity").clone();
        let mut poll = Poll::new(Duration::from_secs(5));
        loop {
            let answer = match poll.read(cancel, self.client.status(&id)).await {
                Ok(answer) => answer,
                Err(reason) => {
                    self.stop(record, journal).await?;
                    return Err(match reason {
                        WaitError::Cancelled => ExecutorError::Cancelled,
                        WaitError::Expired => machine("job exceeded its time limit"),
                    });
                }
            };
            match answer {
                Ok(status) => {
                    if status.job_id != id
                        || status.kind != JobKind::Execution
                        || status
                            .family
                            .as_ref()
                            .is_some_and(|family| family.canonical_job_id != id)
                    {
                        return Err(unresolved("status identity disagrees with the submission"));
                    }
                    if let Some(verdict) = terminal(&status) {
                        journal.terminal = Some(verdict);
                        save(record, journal)?;
                        return Ok(status);
                    }
                    if status.state == crate::client::JobState::Running {
                        poll.running_for(seconds);
                    }
                    let uncertain = status.family.as_ref().is_none_or(|family| {
                        family.partial
                            || matches!(
                                family.logical_state,
                                LogicalState::Indeterminate | LogicalState::Unknown(_)
                            )
                    });
                    if poll.troubled(uncertain, READ_GRACE) {
                        self.stop(record, journal).await?;
                        return Err(unresolved(
                            "family evidence stayed incomplete or indeterminate",
                        ));
                    }
                }
                Err(_) if poll.troubled(true, READ_GRACE) => {
                    self.stop(record, journal).await?;
                    return Err(unresolved("status reads failed beyond their grace period"));
                }
                Err(_) => {}
            }
        }
    }

    async fn stop(&self, record: &Path, journal: &mut Journal) -> Result<Status, ExecutorError> {
        journal.cancel_pending = true;
        save(record, journal)?;
        let id = journal.id.as_ref().expect("submitted identity");
        let status = until_stopped(CANCEL_GRACE, Duration::from_secs(1), || async {
            let _ = tokio::time::timeout(REQUEST_GRACE, self.client.cancel(id)).await;
            if let Ok(Ok(status)) =
                tokio::time::timeout(REQUEST_GRACE, self.client.status(id)).await
                && status.job_id == *id
                && status.kind == JobKind::Execution
                && status
                    .family
                    .as_ref()
                    .is_some_and(|family| family.canonical_job_id == *id)
                && terminal(&status).is_some()
            {
                return Some(status);
            }
            None
        })
        .await
        .map_err(|_| unresolved("cancellation is unconfirmed; retain and replay the journal"))?;
        journal.terminal = terminal(&status);
        save(record, journal)?;
        Ok(status)
    }
}

fn terminal(status: &Status) -> Option<LogicalState> {
    let family = status.family.as_ref()?;
    if family.partial {
        return None;
    }
    match family.logical_state {
        LogicalState::Succeeded | LogicalState::Failed | LogicalState::Cancelled => {
            Some(family.logical_state.clone())
        }
        _ => None,
    }
}

fn save(record: &Path, journal: &Journal) -> Result<(), ExecutorError> {
    let bytes =
        serde_json::to_vec(journal).map_err(|error| std::io::Error::other(error.to_string()))?;
    let mut file = File::create(record.join("request.next"))?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    std::fs::rename(record.join("request.next"), record.join("request.json"))?;
    File::open(record)?.sync_all()?;
    Ok(())
}

fn machine(message: impl Into<String>) -> ExecutorError {
    ExecutorError::Engine {
        engine: "Aruna".into(),
        message: message.into(),
    }
}

fn unresolved(message: &str) -> ExecutorError {
    std::io::Error::other(format!(
        "Aruna ownership or result is unresolved: {message}"
    ))
    .into()
}
