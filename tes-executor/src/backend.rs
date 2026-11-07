//! Runs staged task attempts through TES and collects their output and control files.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::fs::{File, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use executor::poll::{CANCEL_GRACE, Poll, REQUEST_GRACE, WaitError};
use executor::registry::{self, ImageConfigs};
use executor::{Backend, ExecutorError, Finished, Job, wrapper};
use task_model::TaskSpec;
use tokio_util::sync::CancellationToken;

use crate::client::{Client, ExecutorSpec, FileType, Input, Output, Resources, State, Task};
use crate::{ObjectStage, Settings};

/// The folders of a task in its container.
pub(crate) const WORK: &str = "/uwe/work";
pub(crate) const CONTROL: &str = "/uwe/control";
pub(crate) const OUTBOX: &str = "/uwe/outbox";
pub(crate) const HOME: &str = "/uwe/home";
pub(crate) const WRAPPER: &str = "/uwe/wrapper.sh";
pub(crate) const HELPER: &str = "/uwe/helper";

/// How long the backend waits between two looks at a task, at most.
const POLL: Duration = Duration::from_secs(5);

/// How long the request that submits a task may take, as the client allows.
const SUBMIT_TIME: Duration = Duration::from_secs(120);

/// The tag whose value only one submission has, by which a lost submission is found again.
const SUBMISSION: &str = "uwe-submission";

/// How long failed status reads may persist before the backend stops the task.
const READ_GRACE: Duration = Duration::from_secs(120);

/// The TES executor's backend.
#[derive(Debug)]
pub struct TesBackend {
    pub client: Client,
    /// The run whose attempts this backend runs. Each journal names it, so that the tasks of
    /// one run can be found again after its uwe process is gone.
    pub run: String,
    /// Where each attempt gets its scratch directory, which the server reads and writes.
    pub scratch: PathBuf,
    /// The entrypoints and default commands of images, from their registries.
    pub configs: ImageConfigs,
    pub staging: Option<ObjectStage>,
    pub settings: Settings,
}

impl Backend for TesBackend {
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
        if job.spec.output_manifest.is_some() && self.settings.helper_file.is_none() {
            return Err(ExecutorError::Unsupported(
                "TES output manifests require a static uwe binary configured with helper_file"
                    .into(),
            ));
        }
        if job.spec.run_as.is_some() {
            return Err(ExecutorError::Unsupported(
                "TES cannot express task user and group identity".into(),
            ));
        }
        if let Some(task_model::EnvironmentValue::Literal(text)) =
            job.spec.environment.get("UWE_CONTAINER_FLAGS")
        {
            let flags: BTreeMap<String, i64> = serde_json::from_str(text)
                .map_err(|_| ExecutorError::Unsupported("invalid container launch flags".into()))?;
            if flags.contains_key("shm") {
                return Err(ExecutorError::Unsupported(
                    "TES cannot express shared memory sizing".into(),
                ));
            }
        }
        if job
            .spec
            .environment
            .contains_key(executor::daemon::DAEMON_ENV)
        {
            return Err(ExecutorError::Unsupported(
                "TES tasks cannot run as daemons: a TES server gives no address of a running task and keeps no task running for later ones".into(),
            ));
        }
        if job.spec.containers.len() > 1
            || job
                .spec
                .containers
                .iter()
                .any(|container| container.attempts != 1 || !container.after.is_empty())
        {
            return Err(ExecutorError::Unsupported(
                "TES tasks support exactly one executor without container retry dependencies"
                    .into(),
            ));
        }
        if job.placement.offline {
            return Err(ExecutorError::Unsupported(
                "the TES executor cannot run a task without network".into(),
            ));
        }
        if job.placement.accelerators.iter().any(|gpu| gpu.count > 0) {
            return Err(ExecutorError::Unsupported(
                "TES GPU placement is not supported".into(),
            ));
        }
        let peers = [self];
        tokio::select! {
            result = self.admit(&peers) => result?,
            () = cancel.cancelled() => return Err(ExecutorError::Cancelled),
        }
        let root = self.scratch.join(job.name);
        tokio::fs::create_dir_all(&self.scratch).await?;
        tokio::fs::create_dir(&root).await?;
        let _ownership = ownership(&root)?;
        tokio::fs::create_dir_all(root.join("empty")).await?;
        let mut prefix = None;
        let result = self.attempt(job, &root, &mut prefix, cancel).await;
        if let (Some(staging), Some(prefix)) = (&self.staging, &prefix)
            && !root.join("remote.json").try_exists()?
        {
            let journal = serde_json::json!({"backend":"TES","server":self.client.server(),
                "auth":self.client.identity(),"locked":true,"stopped":true,"run":self.run,
                "staging":{"prefix":prefix,"root":staging.objects.url(""),
                    "cleanup":staging.settings.staging_cleanup}});
            journal_at(&root, &journal)?;
        }
        if root.join("remote.json").try_exists()? {
            let journal: serde_json::Value =
                serde_json::from_slice(&std::fs::read(root.join("remote.json"))?)
                    .map_err(|error| unresolved(&root, error))?;
            if journal["stopped"] == true && journal["collect"] != true {
                self.settle_journal(&root, &journal).await?;
            }
        }
        let ended = !root.join("remote.json").exists();
        if result.is_err() && ended {
            let _ = tokio::fs::remove_dir_all(&root).await;
        }
        result
    }
}

impl TesBackend {
    /// Submits the task of an attempt and waits for its end. The prefix of its objects in the
    /// staging root goes to `prefix` before they are there.
    async fn attempt(
        &self,
        job: Job<'_>,
        root: &Path,
        prefix: &mut Option<String>,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        // The command runs through the entrypoint of the image, as `docker run` and the local
        // executor run it; a task without one runs the image's default command.
        let mut spec = job.spec.clone();
        let attempt_key = executor::capture::attempt_prefix(job)?;
        let image = spec.image.as_mut().ok_or(ExecutorError::NoImage)?;
        let config = if image.platform.is_some() {
            let resolved = self
                .configs
                .resolved_for(&image.reference, image.platform.as_deref())
                .await?;
            image.reference = resolved.reference;
            resolved.config
        } else if !spec.replaces_entrypoint || spec.arguments.is_empty() {
            self.configs.get(&image.reference).await?
        } else {
            Default::default()
        };
        if !spec.replaces_entrypoint || spec.arguments.is_empty() {
            spec.arguments = config.command(&job.spec.arguments);
        }
        let shell = crate::staging::shell_command(&job.spec.arguments)
            || job.spec.arguments.is_empty() && crate::staging::shell_command(&config.cmd);
        for container in &mut spec.containers {
            let image = container.image.as_mut().ok_or(ExecutorError::NoImage)?;
            let selected = if image.platform.is_some() {
                let resolved = self
                    .configs
                    .resolved_for(&image.reference, image.platform.as_deref())
                    .await?;
                image.reference = resolved.reference;
                Some(resolved.config)
            } else {
                None
            };
            if container.replaces_entrypoint && !container.arguments.is_empty() {
                continue;
            }
            let config = match selected {
                Some(config) => config,
                None => self.configs.get(&image.reference).await?,
            };
            container.arguments = config.command(&container.arguments);
            if container.arguments.is_empty() {
                return Err(ExecutorError::NoCommand(image.reference.clone()));
            }
        }
        let job = Job { spec: &spec, ..job };
        if self.staging.is_some() {
            *prefix = Some(executor::capture::attempt_prefix(job)?);
        }
        let mut task = match &self.staging {
            Some(staging) => staging.prepare(job, shell).await?,
            None => task(job, root)?,
        };
        if self.staging.is_none() && job.spec.output_manifest.is_some() {
            if task.inputs.iter().any(|input| {
                Path::new(&input.path).starts_with(HELPER)
                    || Path::new(HELPER).starts_with(&input.path)
            }) {
                return Err(ExecutorError::Unsupported(
                    "TES input paths overlap the native helper".into(),
                ));
            }
            task.inputs.push(Input {
                path: HELPER.into(),
                url: file_url(&self.settings.helper_file.as_ref().unwrap().file),
                kind: FileType::File,
            });
        }
        if task.inputs.len() > self.settings.max_inputs
            || task.outputs.len() > self.settings.max_outputs
        {
            return Err(ExecutorError::Unsupported(
                "TES task exceeds the configured input or output limit".into(),
            ));
        }
        task.tags.extend(self.settings.tags.clone());
        platform(&mut task, job, self.settings.platform);
        if let Some(tag) = &self.settings.idempotency_tag {
            task.tags.insert(tag.clone(), attempt_key);
        }
        // The server may take longer than the task, as it stages the files in its time; a
        // task with a shell keeps its own time inside.
        let time = job.placement.time_seconds;
        let timer = time.is_some() && wrapped(&task.executors[0].command);
        if timer {
            let text = format!("{}exit \"$status\"\n", wrapper::timed("sh \"$1\"", time));
            task.executors[0].command = ["sh", "-c", &text, "uwe", WRAPPER]
                .map(str::to_owned)
                .to_vec();
        }
        let deadline = time.map(|seconds| seconds.saturating_add(self.settings.margin().as_secs()));
        let submission = submission_value(root);
        task.tags.insert(SUBMISSION.into(), submission.clone());
        let folders = wrapper::Folders {
            work: &workdir(job),
            control: CONTROL,
            home: HOME,
            outbox: OUTBOX,
            // A TES server runs containers as it likes; the wrapper lets every user change
            // what the task made, for the outbox.
            owner: "",
        };
        if self.staging.is_none() {
            let script = wrapper::wrapper(job.spec, &folders);
            let script = if job.spec.output_manifest.is_some() {
                executor::capture::capture(&script, crate::capture::TRANSFER)?
            } else {
                script
            };
            let script = crate::capture::discover(script, job, &workdir(job), &self.settings)?;
            tokio::fs::write(root.join("wrapper.sh"), script).await?;
        }
        if cancel.is_cancelled() {
            return Err(ExecutorError::Cancelled);
        }
        let host = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
        let started = owner_start(std::process::id())
            .ok_or_else(|| std::io::Error::other("cannot identify the TES coordinator process"))?;
        let mut journal = serde_json::json!({"backend": "TES", "server": self.client.server(),
            "name": job.name, "run": self.run, "id": null, "owner": std::process::id(), "host": host,
            "started": started, "locked": true, "auth": self.client.identity(),
            "submission": submission});
        if let Some(prefix) = prefix {
            journal["staging"] = serde_json::json!({"prefix": prefix,
                "root":self.staging.as_ref().map(|staging| staging.objects.url("")),
                "cleanup":self.staging.as_ref().map(|staging| staging.settings.staging_cleanup)});
            journal["task"] =
                serde_json::to_value(&task).map_err(|error| unresolved(root, error))?;
        }
        if self.settings.idempotency_tag.is_some() {
            journal["request"] =
                serde_json::to_value(&task).map_err(|error| unresolved(root, error))?;
            journal["idempotency_tag"] = serde_json::json!(self.settings.idempotency_tag);
        }
        journal_at(root, &journal)?;
        let id = self
            .client
            .create(&task)
            .await
            .map_err(|error| unresolved(root, error))?;
        journal["id"] = serde_json::json!(id);
        journal_at(root, &journal)?;
        let (status, timed_out) = self.wait(&id, root, deadline, cancel).await?;
        if timed_out {
            tokio::fs::create_dir_all(root.join("control")).await?;
            tokio::fs::write(
                root.join("control/stderr"),
                format!(
                    "uwe: TES stopped the task after its time of {} seconds\n",
                    job.placement.time_seconds.unwrap_or_default()
                ),
            )
            .await?;
            return Ok(Finished {
                address: None,
                exit_code: Some(143),
                files: Vec::new(),
                usage: executor::Usage::of(root),
                root: root.to_owned(),
            });
        }
        let log = status.logs.last().and_then(|log| log.logs.last());
        let ran_out = timer && log.and_then(|log| log.exit_code) == Some(wrapper::TIMED_OUT);
        let finished = self.finish(job, &task, &status, root).await;
        if finished.is_ok() {
            let mut journal: serde_json::Value =
                serde_json::from_slice(&std::fs::read(root.join("remote.json"))?)
                    .map_err(|error| unresolved(root, error))?;
            journal["collect"] = serde_json::json!(false);
            journal_at(root, &journal)?;
        }
        // A task that ended with 124 itself recorded it; the timer ends the wrapper first.
        let recorded = Finished::from_control(root.to_owned()).ok();
        let own = recorded.and_then(|recorded| recorded.exit_code) == Some(wrapper::TIMED_OUT);
        let ran_out = ran_out && !own;
        let ended = matches!(status.state, State::Complete | State::ExecutorError);
        if let (Err(error), Some(_), true) = (&finished, &self.staging, ended) {
            // The objects stay, as they are the only copy of the results.
            tracing::warn!(prefix, %error, "cannot collect the results of an attempt; they stay in the staging root");
        }
        match finished {
            Ok(mut finished) if ran_out => {
                use tokio::io::AsyncWriteExt;
                let note = format!(
                    "uwe: the task ran longer than its time of {} seconds\n",
                    time.unwrap_or_default()
                );
                tokio::fs::create_dir_all(root.join("control")).await?;
                let mut file = tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(root.join("control/stderr"))
                    .await?;
                file.write_all(note.as_bytes()).await?;
                file.flush().await?;
                finished.exit_code = Some(143);
                finished.files.clear();
                Ok(finished)
            }
            finished => finished,
        }
    }

    /// What a task that the server ended left behind, by its state.
    async fn finish(
        &self,
        job: Job<'_>,
        task: &Task,
        status: &crate::client::Status,
        root: &Path,
    ) -> Result<Finished, ExecutorError> {
        if let Some(staging) = &self.staging {
            return match status.state {
                State::Complete | State::ExecutorError => {
                    executor::remote::patiently(|| staging.collect(job, task, status, root)).await
                }
                State::Canceled => Err(ExecutorError::Cancelled),
                _ => Err(failure(status, "the TES server could not run the task")),
            };
        }
        match status.state {
            State::ExecutorError => {
                let mut finished = Finished::from_control(root.to_owned())?;
                finished.exit_code = finished
                    .exit_code
                    .filter(|code| *code != 0)
                    .or_else(|| {
                        status
                            .logs
                            .last()?
                            .logs
                            .last()?
                            .exit_code
                            .filter(|code| *code != 0)
                    })
                    .or(Some(1));
                finished.files.clear();
                Ok(finished)
            }
            State::Complete => {
                let finished = Finished::from_control(root.to_owned())?;
                if finished.exit_code.is_none() {
                    return Err(failure(status, "the task ended without an exit code"));
                }
                if finished.exit_code == Some(0) && job.spec.output_manifest.is_some() {
                    executor::capture::arrange(&finished).await?;
                }
                Ok(finished)
            }
            State::Canceled => Err(ExecutorError::Cancelled),
            _ => Err(failure(status, "the TES server could not run the task")),
        }
    }

    /// Resolves submissions whose coordinator no longer runs before accepting a new run.
    pub async fn reconcile(&self, peers: &[&TesBackend]) -> Result<(), ExecutorError> {
        let mut scan = self.client.history.lock().await;
        scan.reset();
        while !scan.complete() {
            let entries = scan.page(&self.scratch, 128)?;
            self.recover(peers, entries).await?;
        }
        Ok(())
    }

    async fn admit(&self, peers: &[&TesBackend]) -> Result<(), ExecutorError> {
        let mut scan = self.client.history.lock().await;
        let entries = scan.page(&self.scratch, 128)?;
        let result = self.recover(peers, entries).await;
        if result.is_err() {
            scan.reset();
        }
        result
    }

    async fn recover(
        &self,
        peers: &[&TesBackend],
        entries: Vec<PathBuf>,
    ) -> Result<(), ExecutorError> {
        if entries.is_empty() {
            return Ok(());
        }
        tokio::time::timeout(CANCEL_GRACE, async {
            std::fs::create_dir_all(&self.scratch)?;
            let gate = File::options()
                .write(true)
                .create(true)
                .truncate(false)
                .open(self.scratch.join("remote-recovery.lock"))?;
            loop {
                match gate.try_lock() {
                    Ok(()) => break,
                    Err(TryLockError::WouldBlock) => {
                        tokio::time::sleep(Duration::from_millis(10)).await
                    }
                    Err(TryLockError::Error(error)) => return Err(error.into()),
                }
            }
            self.reconcile_records(peers, entries).await
        })
        .await
        .map_err(|_| unresolved(&self.scratch, "admission recovery timed out"))?
    }

    async fn reconcile_records(
        &self,
        peers: &[&TesBackend],
        entries: Vec<PathBuf>,
    ) -> Result<(), ExecutorError> {
        let host = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
        for root in entries {
            tokio::task::yield_now().await;
            let path = root.join("remote.json");
            if !path.try_exists()? {
                continue;
            }
            let mut journal: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)
                .map_err(|error| unresolved(&root, error))?;
            if journal["backend"] != "TES" {
                return Err(unresolved(
                    &root,
                    "the original backend or coordinator host changed",
                ));
            }
            if !peers
                .iter()
                .any(|peer| Some(peer.client.server()) == journal["server"].as_str())
            {
                continue;
            }
            let live_owner = journal["host"].as_str() == Some(host.as_str())
                && journal["owner"]
                    .as_u64()
                    .and_then(|pid| u32::try_from(pid).ok())
                    .and_then(owner_start)
                    .as_deref()
                    == journal["started"].as_str()
                && journal["started"].is_string();
            let _ownership = if journal["locked"] == true {
                let lock = match File::options().write(true).open(root.join("remote.lock")) {
                    Ok(lock) => lock,
                    Err(_) if !path.try_exists()? => continue,
                    Err(error) => return Err(unresolved(&root, error)),
                };
                match lock.try_lock() {
                    Err(TryLockError::WouldBlock) => continue,
                    Err(TryLockError::Error(error)) => return Err(unresolved(&root, error)),
                    Ok(()) => lock,
                }
            } else {
                if journal["host"].as_str() != Some(host.as_str()) {
                    return Err(unresolved(&root, "the legacy coordinator host changed"));
                }
                if live_owner {
                    continue;
                }
                File::open(&path)?
            };
            if !path.try_exists()? {
                continue;
            }
            journal = serde_json::from_slice(&std::fs::read(&path)?)
                .map_err(|error| unresolved(&root, error))?;
            if journal["backend"] != "TES" {
                return Err(unresolved(
                    &root,
                    "the original backend or coordinator host changed",
                ));
            }
            let Some(backend) = peers.iter().find(|peer| {
                Some(peer.client.server()) == journal["server"].as_str()
                    && journal["auth"]
                        .as_str()
                        .is_none_or(|auth| auth == peer.client.identity())
            }) else {
                // Another site owns this journal; its admission or aggregate recovery handles it.
                continue;
            };
            if journal["stopped"] == true {
                backend.settle_journal(&root, &journal).await?;
                continue;
            }
            if journal["id"].is_null() {
                let tag = backend.settings.idempotency_tag.as_deref().filter(|tag| {
                    journal["idempotency_tag"].as_str() == Some(*tag)
                        && journal["auth"].as_str() == Some(backend.client.identity().as_str())
                });
                if let Some(tag) = tag {
                    let task: Task = serde_json::from_value(journal["request"].clone())
                        .map_err(|error| unresolved(&root, error))?;
                    if !task.tags.contains_key(tag) {
                        return Err(unresolved(
                            &root,
                            "the saved submission has no idempotency tag",
                        ));
                    }
                    let id = backend
                        .client
                        .create(&task)
                        .await
                        .map_err(|error| unresolved(&root, error))?;
                    journal["id"] = serde_json::json!(id);
                    journal_at(&root, &journal)?;
                }
            }
            // A submission without an answer must not block every later run.
            if journal["id"].as_str().is_none_or(str::is_empty) {
                match backend.identify(&root, &journal).await {
                    Submitted::Found(id) => {
                        journal["id"] = serde_json::json!(id);
                        journal_at(&root, &journal)?;
                    }
                    Submitted::Absent => {
                        tracing::warn!(attempt = %root.display(),
                            "the task of an unclear submission is not on the TES server; the attempt is lost");
                        journal["stopped"] = serde_json::json!(true);
                        journal_at(&root, &journal)?;
                        backend.settle_journal(&root, &journal).await?;
                        continue;
                    }
                    Submitted::Unclear(reason) => {
                        tracing::warn!(attempt = %root.display(), reason,
                            "cannot resolve an unclear submission yet");
                        continue;
                    }
                }
            }
            let id = journal["id"].as_str().unwrap_or_default();
            backend.stop(id, &root, CANCEL_GRACE).await?;
            let journal = serde_json::from_slice(&std::fs::read(&path)?)
                .map_err(|error| unresolved(&root, error))?;
            backend.settle_journal(&root, &journal).await?;
        }
        Ok(())
    }

    /// Removes the objects in the staging root that the journal of an attempt names.
    async fn settle_journal(
        &self,
        root: &Path,
        journal: &serde_json::Value,
    ) -> Result<(), ExecutorError> {
        let prefix = journal["staging"]["prefix"].as_str();
        if let Some(prefix) = prefix {
            let staging = self
                .staging
                .as_ref()
                .ok_or_else(|| unresolved(root, "the staging configuration is missing"))?;
            if journal["staging"]["root"].as_str() != Some(staging.objects.url("").as_str()) {
                return Err(unresolved(root, "the staging root changed"));
            }
            if journal["collect"] == true {
                let task = serde_json::from_value(journal["task"].clone())
                    .map_err(|error| unresolved(root, error))?;
                let status = serde_json::from_value(journal["status"].clone())
                    .map_err(|error| unresolved(root, error))?;
                staging.collect_prefix(prefix, &task, &status, root).await?;
                let mut journal = journal.clone();
                journal["collect"] = serde_json::json!(false);
                journal_at(root, &journal)?;
            }
            let mut staging = staging.clone();
            staging.settings.staging_cleanup = journal["staging"]["cleanup"]
                .as_bool()
                .unwrap_or(staging.settings.staging_cleanup);
            staging.try_clean(prefix).await?;
        }
        std::fs::remove_file(root.join("remote.json"))?;
        File::open(root)?.sync_all()?;
        Ok(())
    }

    /// Finds the task of a journal whose submission left no id, by the tag value that only
    /// that submission has, with the credentials that made it.
    async fn identify(&self, root: &Path, journal: &serde_json::Value) -> Submitted {
        // The submission may take as long as its request may.
        let age = POLL * 4 + SUBMIT_TIME + REQUEST_GRACE;
        if !executor::poll::old_enough(&root.join("remote.json"), age) {
            return Submitted::Unclear("the submission is too recent to tell");
        }
        // A task counts as absent only when two answers in a row lack it.
        match self.look_up(journal).await {
            Submitted::Absent => {
                tokio::time::sleep(POLL).await;
                self.look_up(journal).await
            }
            found => found,
        }
    }

    /// One answer of the server about the task of a journal.
    async fn look_up(&self, journal: &serde_json::Value) -> Submitted {
        let (Some(name), Some(value)) = (journal["name"].as_str(), journal["submission"].as_str())
        else {
            return Submitted::Unclear("the journal names no submission tag");
        };
        if journal["auth"].as_str() != Some(self.client.identity().as_str()) {
            return Submitted::Unclear("the credentials of the TES server changed");
        }
        let listed = self.client.list(name, SUBMISSION, value);
        let tasks = match tokio::time::timeout(REQUEST_GRACE, listed).await {
            Ok(Ok(tasks)) => tasks,
            _ => return Submitted::Unclear("the TES server cannot list the tasks"),
        };
        // Only a task with the tag value of this submission is ours.
        let mut ours = (tasks.into_iter())
            .filter(|task| task.tags.get(SUBMISSION).map(String::as_str) == Some(value));
        match (ours.next(), ours.next()) {
            (None, _) => Submitted::Absent,
            (Some(task), None) => Submitted::Found(task.id),
            (Some(_), Some(_)) => Submitted::Unclear("several tasks have the submission tag"),
        }
    }

    /// Waits through transient reads, stopping accepted work on cancellation or timeout.
    async fn wait(
        &self,
        id: &str,
        root: &Path,
        seconds: Option<u64>,
        cancel: &CancellationToken,
    ) -> Result<(crate::client::Status, bool), ExecutorError> {
        let mut poll = Poll::new(POLL);
        loop {
            tracing::debug!(task = id, "polling TES task");
            let answer = match poll.read(cancel, self.client.progress(id)).await {
                Ok(answer) => answer,
                Err(WaitError::Cancelled) => {
                    self.stop(id, root, CANCEL_GRACE).await?;
                    return Err(ExecutorError::Cancelled);
                }
                Err(WaitError::Expired) => {
                    return self
                        .stop(id, root, CANCEL_GRACE)
                        .await
                        .map(|status| (status, true));
                }
            };
            match answer {
                Ok(status) => {
                    poll.troubled(false, READ_GRACE);
                    if status.state.is_final() {
                        record_stop(root, &status)?;
                        return Ok((status, false));
                    }
                    // Some servers report UNKNOWN for a task that runs, or for good, so the time
                    // of the task starts then too.
                    if matches!(status.state, State::Running | State::Unknown) {
                        poll.running_for(seconds);
                    }
                }
                Err(error) => {
                    if poll.troubled(true, READ_GRACE) {
                        self.stop(id, root, CANCEL_GRACE).await?;
                        return Err(machine(error));
                    }
                }
            }
        }
    }

    /// Cancellation is complete only when the server reports a terminal state.
    async fn stop(
        &self,
        id: &str,
        root: &Path,
        grace: Duration,
    ) -> Result<crate::client::Status, ExecutorError> {
        let status = executor::poll::until_stopped(grace, Duration::from_secs(1), || async {
            let request_grace = grace.min(REQUEST_GRACE);
            let _ = tokio::time::timeout(request_grace, self.client.cancel(id)).await;
            if let Ok(Ok(status)) =
                tokio::time::timeout(request_grace, self.client.status(id)).await
                && status.state.is_final()
            {
                return Some(status);
            }
            None
        })
        .await
        .map_err(|_| unresolved(root, format!("task {id} termination was not confirmed")))?;
        record_stop(root, &status)?;
        Ok(status)
    }
}

/// Persists terminal evidence before collection or cleanup can begin.
fn record_stop(root: &Path, status: &crate::client::Status) -> Result<(), ExecutorError> {
    let mut journal: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("remote.json"))?)
            .map_err(|error| unresolved(root, error))?;
    journal["stopped"] = serde_json::json!(true);
    if journal["staging"].is_object()
        && matches!(status.state, State::Complete | State::ExecutorError)
    {
        journal["collect"] = serde_json::json!(true);
        journal["status"] =
            serde_json::to_value(status).map_err(|error| unresolved(root, error))?;
    }
    journal_at(root, &journal)
}

/// What became of a submission whose answer was lost.
enum Submitted {
    /// The task with this id.
    Found(String),
    /// The server has no such task: it never took it.
    Absent,
    /// The server cannot tell yet, for this reason.
    Unclear(&'static str),
}

/// A value that only the submission of the attempt at `root` has.
fn submission_value(root: &Path) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let host = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap_or_default();
    let text = format!(
        "{host}\0{}\0{nanos}\0{}",
        std::process::id(),
        root.display()
    );
    task_model::Digest::sha256(text.as_bytes()).hex().to_owned()
}

fn ownership(root: &Path) -> Result<File, ExecutorError> {
    let lock = File::options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("remote.lock"))?;
    lock.try_lock()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(lock)
}

fn owner_start(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, fields) = stat.rsplit_once(')')?;
    if matches!(fields.split_whitespace().next()?, "Z" | "X") {
        return None;
    }
    fields.split_whitespace().nth(19).map(str::to_owned)
}

fn unresolved(root: &Path, error: impl std::fmt::Display) -> ExecutorError {
    std::io::Error::other(format!(
        "TES ownership is unresolved: {error}; retained {}",
        root.join("remote.json").display()
    ))
    .into()
}

fn journal_at(root: &Path, journal: &serde_json::Value) -> Result<(), ExecutorError> {
    let path = root.join("remote.json.next");
    let mut file = std::fs::File::create(&path)?;
    file.write_all(journal.to_string().as_bytes())?;
    file.sync_all()?;
    std::fs::rename(path, root.join("remote.json"))?;
    std::fs::File::open(root)?.sync_all()?;
    Ok(())
}

/// The work directory of a task in its container.
fn workdir(job: Job<'_>) -> String {
    match job.spec.run_as {
        Some(_) => WORK.to_owned(),
        None => job.spec.workdir.clone().unwrap_or_else(|| WORK.to_owned()),
    }
}

/// Where a container starts: in the work directory `work`, or, for a task that runs as its
/// image says, in `workdir` or the working directory of the image (`None`).
fn start(spec: &TaskSpec, workdir: Option<&str>, work: &str) -> Option<String> {
    match spec.run_as {
        Some(_) => workdir.map(str::to_owned),
        None => Some(work.to_owned()),
    }
}

/// The TES task of an attempt whose scratch directory is `root`.
fn task(job: Job<'_>, root: &Path) -> Result<Task, ExecutorError> {
    let spec = job.spec;
    let image = spec.image.as_ref().ok_or(ExecutorError::NoImage)?;
    // TES needs a command: the task's, or the default command of the image.
    if spec.arguments.is_empty() {
        return Err(ExecutorError::NoCommand(image.reference.clone()));
    }
    let work = workdir(job);
    let inside = |path: &str| match path.trim_end_matches('/') {
        "." => work.clone(),
        absolute if absolute.starts_with('/') => absolute.to_owned(),
        relative => format!("{}/{relative}", work.trim_end_matches('/')),
    };
    let mut inputs = vec![Input {
        path: WRAPPER.into(),
        url: file_url(&root.join("wrapper.sh")),
        kind: FileType::File,
    }];
    // An input that the task changes where it is goes back to its place as an output too.
    let mut written_back = Vec::new();
    for staged in &job.staging.inputs {
        let kind = match staged.directory {
            true => FileType::Directory,
            false => FileType::File,
        };
        inputs.push(Input {
            path: inside(&staged.path),
            url: file_url(&staged.source),
            kind,
        });
        if staged.in_place {
            written_back.push(Output {
                path: inside(&staged.path),
                url: file_url(&staged.source),
                kind,
                path_prefix: None,
            });
        }
    }
    // A directory that the task gets empty is an empty folder of the scratch directory.
    for directory in &job.staging.directories {
        inputs.push(Input {
            path: inside(directory),
            url: file_url(&root.join("empty")),
            kind: FileType::Directory,
        });
    }
    if let Some(stdin) = &job.staging.stdin {
        inputs.push(Input {
            path: format!("{CONTROL}/{}", wrapper::STDIN),
            url: file_url(stdin),
            kind: FileType::File,
        });
    }
    let mut outputs = vec![
        Output {
            path: OUTBOX.into(),
            url: file_url(&root.join("work")),
            kind: FileType::Directory,
            path_prefix: None,
        },
        Output {
            path: CONTROL.into(),
            url: file_url(&root.join("control")),
            kind: FileType::Directory,
            path_prefix: None,
        },
    ];
    if spec.output_manifest.is_some() {
        outputs.push(Output {
            path: crate::capture::TRANSFER.into(),
            url: file_url(&root.join(executor::helper::TRANSFER)),
            kind: FileType::Directory,
            path_prefix: None,
        });
    }
    outputs.extend(written_back);
    let placement = job.placement;
    let resources = Resources {
        cpu_cores: Some(placement.cpus.max(1)),
        ram_gb: placement
            .memory_bytes
            .map(|bytes| bytes as f64 / crate::client::GB),
        disk_gb: placement
            .disk_bytes
            .map(|bytes| bytes as f64 / crate::client::GB),
        ..Resources::default()
    };
    Ok(Task {
        name: job.name.to_owned(),
        inputs,
        outputs,
        resources: Some(resources),
        executors: vec![ExecutorSpec {
            image: image.reference.clone(),
            command: vec!["sh".into(), WRAPPER.into()],
            workdir: start(spec, spec.workdir.as_deref(), &work),
            env: BTreeMap::new(),
        }],
        // The work directory, the home folder and `/tmp` are writable, also when the image's
        // own files are not.
        volumes: vec![
            work,
            HOME.into(),
            "/tmp".into(),
            CONTROL.into(),
            OUTBOX.into(),
        ],
        tags: BTreeMap::from([("uwe-attempt".to_owned(), job.name.to_owned())]),
    })
}

/// What the placement of a task and the platform of the server add to it: spot VMs, and for
/// the Tencent Omics platform its tags and the GPUs of resource labels or accelerators.
fn platform(task: &mut Task, job: Job<'_>, platform: Option<crate::Platform>) {
    let placement = job.placement;
    let resources = task.resources.get_or_insert_with(Resources::default);
    if placement.preemptible {
        resources.preemptible = Some(true);
    }
    if platform != Some(crate::Platform::TencentOmics) {
        return;
    }
    let labels = &placement.labels;
    if labels.get("preemptible").map(String::as_str) == Some("true") {
        resources.preemptible = Some(true);
    }
    let count: u32 = placement.accelerators.iter().map(|gpu| gpu.count).sum();
    let kind = (placement
        .accelerators
        .iter()
        .find_map(|gpu| gpu.kind.clone()))
    .or_else(|| {
        let named = placement.options.get("accelerator");
        named
            .and_then(|named| named["type"].as_str())
            .map(str::to_owned)
    });
    let count = (count > 0).then(|| count.to_string());
    for (key, value) in [("gpuType", kind), ("gpuCount", count)] {
        if let Some(value) = labels.get(key).cloned().or(value) {
            resources.backend_parameters.insert(key.to_owned(), value);
        }
    }
    // The platform sets RUN_ID for the runs that it starts, as nf-tencentcloud reads it.
    if let Ok(run) = std::env::var("RUN_ID") {
        task.tags.insert("runUuid".into(), run);
    }
    if let Some(folder) = task
        .executors
        .first()
        .and_then(|first| first.workdir.clone())
    {
        task.tags.insert("WorkingDir".into(), folder);
    }
    if let Some(output) = task.outputs.first() {
        task.tags.insert("DestDir".into(), output.url.clone());
    }
}

/// Whether a command runs the wrapper of the task with a shell, plainly or under a timer.
pub(crate) fn wrapped(command: &[String]) -> bool {
    command.first().is_some_and(|program| program == "sh")
        && command.last().is_some_and(|path| path == WRAPPER)
}

/// A `file://` URL of a local path, with the characters that have a meaning in URLs encoded.
fn file_url(path: &Path) -> String {
    let mut url = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'.' | b'_' | b'~' => {
                url.push(char::from(byte))
            }
            _ => url.push_str(&format!("%{byte:02X}")),
        }
    }
    url
}

/// A failure of the server, which another attempt may not have.
fn machine(error: crate::TesError) -> ExecutorError {
    ExecutorError::Engine {
        engine: "TES".into(),
        message: error.to_string(),
    }
}

/// A task that the server could not finish, with what its logs say: the lines about errors,
/// or else the last lines.
fn failure(status: &crate::client::Status, message: &str) -> ExecutorError {
    let all: Vec<&str> = (status.logs.iter())
        .flat_map(|log| log.system_logs.iter().map(String::as_str))
        .collect();
    let errors: Vec<&str> = (all.iter().copied())
        .filter(|line| line.to_lowercase().contains("error"))
        .collect();
    let logs = match errors.is_empty() {
        true => &all[all.len().saturating_sub(5)..],
        false => &errors[..],
    };
    ExecutorError::Engine {
        engine: "TES".into(),
        message: match logs.is_empty() {
            true => format!("{message} ({:?})", status.state),
            false => format!("{message} ({:?}): {}", status.state, logs.join("; ")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use executor::{Staged, Staging};
    use task_model::{FORMAT, Image, OutputRule, Placement, TaskSpec};

    #[tokio::test]
    async fn retries_stopped_collection() {
        let root = tempfile::tempdir().unwrap();
        let operator = opendal::Operator::new(opendal::services::Memory::default()).unwrap();
        let objects = content_store::Store::remote(operator.clone(), "s3://bucket/stage").unwrap();
        let settings = Settings {
            staging_cleanup: true,
            ..Settings::default()
        };
        let backend = TesBackend {
            run: String::new(),
            client: Client::new("http://127.0.0.1:1", None).unwrap(),
            scratch: root.path().into(),
            configs: Default::default(),
            staging: Some(
                ObjectStage::new(
                    content_store::Store::memory().unwrap(),
                    objects.clone(),
                    settings.clone(),
                )
                .unwrap(),
            ),
            settings,
        };
        let task = Task {
            executors: vec![ExecutorSpec {
                command: vec!["sh".into(), WRAPPER.into()],
                ..ExecutorSpec::default()
            }],
            ..Task::default()
        };
        let journal = serde_json::json!({"stopped":true,"collect":true,"task":task,
            "status":{"id":"done","state":"COMPLETE"},
            "staging":{"prefix":"attempts/test","root":objects.url(""),"cleanup":true}});
        journal_at(root.path(), &journal).unwrap();
        assert!(backend.settle_journal(root.path(), &journal).await.is_err());
        assert!(root.path().join("remote.json").exists());
        for (name, text) in [("exit", "0\n"), ("manifest", ""), ("objects", "")] {
            operator
                .write(&format!("attempts/test/control/{name}"), text.to_owned())
                .await
                .unwrap();
        }
        backend.settle_journal(root.path(), &journal).await.unwrap();
        assert!(!root.path().join("remote.json").exists());
        assert!(objects.objects("attempts/test/").await.unwrap().is_empty());
    }

    #[test]
    fn termination_preserves_journal() {
        let folder = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let server = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let thread = std::thread::spawn(move || {
            use std::io::{BufRead, Read};
            let until = std::time::Instant::now() + Duration::from_millis(300);
            let mut calls = Vec::new();
            while std::time::Instant::now() < until {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                };
                stream
                    .set_read_timeout(Some(Duration::from_millis(100)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(&mut stream);
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let mut length = 0;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header.trim().is_empty() {
                        break;
                    }
                    if let Some((key, value)) = header.split_once(':')
                        && key.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().unwrap();
                    }
                }
                reader.read_exact(&mut vec![0; length]).unwrap();
                calls.push(first.clone());
                if first.starts_with("POST ") {
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                        )
                        .unwrap();
                } else {
                    std::thread::sleep(Duration::from_millis(150));
                }
            }
            calls
        });
        let backend = TesBackend {
            run: String::new(),
            client: Client::new(&server, None).unwrap(),
            scratch: folder.path().to_owned(),
            configs: ImageConfigs::default(),
            staging: None,
            settings: Settings::default(),
        };
        journal_at(folder.path(), &serde_json::json!({"id": "accepted"})).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let error = runtime
            .block_on(backend.stop("accepted", folder.path(), Duration::from_millis(50)))
            .expect_err("a stalled response cannot confirm termination");
        assert!(!error.retryable());
        assert!(folder.path().join("remote.json").exists());
        let calls = thread.join().unwrap();
        assert!(
            calls
                .first()
                .unwrap()
                .starts_with("POST /ga4gh/tes/v1/tasks/accepted:cancel ")
        );
        assert!(calls.iter().any(|call| call.starts_with("GET ")));
    }

    #[test]
    fn builds_tes_tasks() {
        let spec = TaskSpec {
            run_as: None,
            format: FORMAT,
            namespace: String::new(),
            image: Some(Image {
                reference: "debian@sha256:ab".into(),
                platform: None,
            }),
            arguments: vec!["bash".into(), "-c".into(), "true".into()],
            workdir: None,
            environment: BTreeMap::new(),
            inputs: Vec::new(),
            outputs: vec![OutputRule {
                pattern: "out.txt".into(),
            }],
            output_manifest: None,
            stdin: None,
            containers: Vec::new(),
            replaces_entrypoint: false,
        };
        let staging = Staging {
            inputs: vec![Staged {
                path: "in dir/a.txt".into(),
                source: "/store/runs/x/a.txt".into(),
                directory: false,
                copy: false,
                executable: false,
                ..Default::default()
            }],
            directories: vec!["made".into()],
            stdin: None,
        };
        let placement = Placement {
            cpus: 2,
            memory_bytes: Some(3_000_000_000),
            ..Placement::default()
        };
        let job = Job {
            name: "run-P-1-1",
            spec: &spec,
            staging: &staging,
            placement: &placement,
        };
        let task = task(job, Path::new("/scratch/run-P-1-1")).expect("a task");
        let paths: Vec<(&str, &str)> = (task.inputs.iter())
            .map(|input| (input.path.as_str(), input.url.as_str()))
            .collect();
        let expected = [
            ("/uwe/wrapper.sh", "file:///scratch/run-P-1-1/wrapper.sh"),
            ("/uwe/work/in dir/a.txt", "file:///store/runs/x/a.txt"),
            ("/uwe/work/made", "file:///scratch/run-P-1-1/empty"),
        ];
        assert_eq!(paths, expected);
        assert_eq!(task.outputs[0].url, "file:///scratch/run-P-1-1/work");
        assert!(task.volumes.contains(&CONTROL.to_owned()));
        assert!(task.volumes.contains(&OUTBOX.to_owned()));
        let resources = task.resources.expect("resources");
        assert_eq!(
            (resources.cpu_cores, resources.ram_gb),
            (Some(2), Some(3.0))
        );
        assert_eq!(file_url(Path::new("/a b/c")), "file:///a%20b/c");
    }
}
