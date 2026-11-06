//! Runs attempts as batch jobs of a scheduler that uwe drives with its command line tools, like
//! HTCondor and PBS, with the job scripts of the SLURM jobs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::future::Future;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use executor::manifest::parse;
use executor::poll::{CANCEL_GRACE, Poll};
use executor::registry::{self, ImageConfigs};
use executor::wrapper::{MANIFEST, STDERR};
use executor::{
    Access, Backend, CONTROL, ContainerRun, ExecutorError, Finished, Job, containers, place,
};
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::backend::{explain, read_number, record, sync_files};
use crate::script::{
    CONTAINER_EXIT, Container, NOT_SEEN, SCRIPT, TIMED_OUT, check_binds, check_gpus, engine_marker,
    job_script, shared_memory, size_memory,
};

/// How long one call of a tool of the scheduler may take, as a busy scheduler answers slowly.
pub const TOOL_GRACE: Duration = Duration::from_secs(60);

/// How long a job that ended may take to show the record of its container, which a shared file
/// system may show late.
const EXIT_GRACE: Duration = Duration::from_secs(30);

/// How long a job whose container ended may stay in the queue, while the scheduler copies its
/// output back and writes its summary.
const END_GRACE: Duration = Duration::from_secs(120);

/// A scheduler of batch jobs that uwe drives with its command line tools.
pub trait Scheduler: Send + Sync + 'static {
    /// Its name in messages, like `HTCondor`.
    const NAME: &'static str;

    /// The lines that the job script gets after its first line, like `#PBS` options.
    fn head(&self, _submission: &Submission<'_>) -> String {
        String::new()
    }

    /// Submits the job script of an attempt and gives the id of its job.
    fn submit(
        &self,
        submission: &Submission<'_>,
    ) -> impl Future<Output = Result<String, String>> + Send;

    /// What the scheduler tells of these jobs; a job that it does not know is left out.
    fn states(
        &self,
        ids: &[String],
    ) -> impl Future<Output = Result<HashMap<String, State>, String>> + Send;

    /// Removes a job from the queue.
    fn remove(&self, id: &str) -> impl Future<Output = Result<(), String>> + Send;

    /// Reads what the scheduler wrote about a job that left the queue, in its attempt folder.
    fn ended(&self, _root: &Path) -> std::io::Result<()> {
        Ok(())
    }
}

/// What the job of an attempt asks the scheduler for.
#[derive(Clone, Copy, Debug)]
pub struct Submission<'a> {
    pub job: Job<'a>,
    /// The folder of the attempt on this machine.
    pub root: &'a Path,
    /// The job script, in the control folder of the attempt.
    pub script: &'a Path,
    /// Whether the nodes share no folders with this machine, so that the job gets its files
    /// through the staging root and keeps no log in the attempt folder.
    pub remote: bool,
    /// How much longer than its task the job may run, in seconds.
    pub margin: u64,
    /// The web proxy of the job, when the nodes reach the web only through one.
    pub proxy: Option<&'a str>,
}

/// What the scheduler tells of a job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Waiting for a node, running, or ending.
    Active,
    /// Held for this reason; the scheduler will not run it on its own.
    Held(String),
    /// Ended, in the scheduler's words, like `removed`.
    Ended(String),
}

/// A backend whose attempts run as jobs of the scheduler `S`, on nodes that see the scratch
/// folder at the same path.
#[derive(Debug)]
pub struct GridBackend<S> {
    pub scheduler: S,
    /// Where each attempt gets its folder.
    pub scratch: PathBuf,
    pub container: Container,
    /// How often the scheduler is asked about the jobs, at most.
    pub poll: Duration,
    /// How much longer than its task a job may run, for staging and the start of its container.
    pub staging_time: Duration,
    pub proxy: Option<String>,
    pub configs: ImageConfigs,
    queue: Queue,
}

/// How a job ended.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// The job script recorded the exit code of the container.
    Done(i32),
    /// The job left the queue without that record, in this state when the scheduler tells it.
    Stopped(Option<String>),
    /// The scheduler held the job for this reason; uwe removes it.
    Held(String),
}

impl<S: Scheduler> GridBackend<S> {
    /// A backend whose attempts get their folders in `scratch` and run their tasks with
    /// `container`.
    pub fn new(scheduler: S, scratch: PathBuf, container: Container) -> GridBackend<S> {
        GridBackend {
            scheduler,
            scratch,
            container,
            poll: Duration::from_secs(10),
            staging_time: Duration::from_secs(5 * 60),
            proxy: None,
            configs: ImageConfigs::default(),
            queue: Queue::default(),
        }
    }
}

impl<S: Scheduler + std::fmt::Debug> Backend for GridBackend<S> {
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
        self.admit(job)?;
        tokio::fs::create_dir_all(&self.scratch).await?;
        let root = self.scratch.join(job.name);
        let staging = job.staging.clone();
        let folder = root.clone();
        let mounts = tokio::task::spawn_blocking(move || place(&folder, &staging))
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let mounts = match mounts {
            // Another attempt has this name; its folder is not ours to remove.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(error.into());
            }
            Err(error) => {
                let _ = tokio::fs::remove_dir_all(&root).await;
                return Err(error.into());
            }
            Ok(mounts) => mounts,
        };
        let result = self.attempt(job, &root, &mounts, cancel).await;
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&root).await;
        }
        let _ = tokio::fs::remove_file(engine_marker(&root)).await;
        result
    }
}

impl<S: Scheduler> GridBackend<S> {
    /// Refuses what the jobs cannot do, before anything is placed.
    fn admit(&self, job: Job<'_>) -> Result<(), ExecutorError> {
        let refuse = |message: String| Err(ExecutorError::Unsupported(message));
        check_gpus(job.placement)?;
        let gpus = job.placement.accelerators.iter().any(|gpu| gpu.count > 0);
        if gpus && matches!(&self.container, Container::Engine(_)) {
            return refuse(format!("{} GPU placement requires Apptainer", S::NAME));
        }
        if job.placement.offline
            && matches!(&self.container, Container::Apptainer(program) if !program.ends_with("apptainer"))
        {
            return refuse(format!(
                "the {} Singularity executor cannot run a task without network",
                S::NAME
            ));
        }
        let spec = job.spec;
        if spec.environment.contains_key(executor::daemon::DAEMON_ENV) {
            return refuse(format!("{} jobs cannot run daemons", S::NAME));
        }
        if !spec.containers.is_empty() {
            return refuse(format!(
                "the {} executor runs only tasks of one container",
                S::NAME
            ));
        }
        let image = spec.image.as_ref().ok_or(ExecutorError::NoImage)?;
        let reference = &image.reference;
        if reference.starts_with("docker-archive:") || reference.starts_with("sha256:") {
            return refuse(format!(
                "the {} executor runs images of registries and SIF files, not image archives",
                S::NAME
            ));
        }
        Ok(())
    }

    /// The command that a container of `image` runs for these arguments.
    async fn task_command(
        &self,
        image: &mut task_model::Image,
        replaces: bool,
        arguments: &[String],
    ) -> Result<Vec<String>, ExecutorError> {
        let reference = &image.reference;
        let registry = !reference.starts_with('/')
            && !reference.ends_with(".sif")
            && !reference.contains("://");
        let own = replaces && !arguments.is_empty();
        let config = match (image.platform.as_deref(), registry, own) {
            (Some(_), false, _) => {
                return Err(ExecutorError::Unsupported(format!(
                    "{} cannot select a platform for a SIF file or image address",
                    S::NAME
                )));
            }
            (Some(platform), true, _) => {
                let resolved = self.configs.resolved_for(reference, Some(platform)).await?;
                image.reference = resolved.reference;
                Some(resolved.config)
            }
            (None, true, false) => Some(self.configs.get(reference).await?),
            (None, _, _) => None,
        };
        let command = match config {
            Some(config) if !own => config.command(arguments),
            _ => arguments.to_vec(),
        };
        if command.is_empty() {
            return Err(ExecutorError::NoCommand(image.reference.clone()));
        }
        Ok(command)
    }

    /// Writes the wrapper and the job script of a placed attempt, submits the job and waits
    /// for its end.
    async fn attempt(
        &self,
        job: Job<'_>,
        root: &Path,
        mounts: &[executor::process::Mount],
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        let mut spec = job.spec.clone();
        let shm = shared_memory(&self.container, &spec)?;
        let (replaces, arguments) = (spec.replaces_entrypoint, spec.arguments.clone());
        let image = spec.image.as_mut().ok_or(ExecutorError::NoImage)?;
        spec.arguments = self.task_command(image, replaces, &arguments).await?;
        let spec = &spec;
        let image = spec.image.as_ref().ok_or(ExecutorError::NoImage)?;
        let control = root.join(CONTROL);
        let engine = matches!(&self.container, Container::Engine(_));
        if engine {
            // The container can write its folder, so its removal proof stays outside it.
            tokio::fs::write(engine_marker(root), "0\n").await?;
        }
        let owner = tokio::fs::metadata(root).await?;
        let access = match &self.container {
            // Apptainer runs every container as the user who runs uwe.
            Container::Apptainer(_) => Access {
                run_as: spec.run_as,
                ..Access::default()
            },
            Container::Engine(engine) => {
                Access::of((owner.uid(), owner.gid()), engine.rootless, spec.run_as, "")
            }
        };
        if access.hand_back.is_some() {
            executor::access::open(root)?;
        }
        let name = format!("uwe-{}", job.name);
        let run = ContainerRun {
            name: &name,
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
        let gpus = job.placement.accelerators.iter().any(|gpu| gpu.count > 0);
        check_binds(&self.container, &run)?;
        let time = job.placement.time_seconds;
        let wrapper = run.wrapper(spec);
        tokio::fs::write(run.script(), &wrapper).await?;
        let script = job_script(&self.container, &run, gpus, time, wrapper.len() as u64);
        let script = size_memory(&self.container, script, shm);
        let path = control.join(SCRIPT);
        let submission = Submission {
            job,
            root,
            script: &path,
            remote: false,
            margin: self.staging_time.as_secs(),
            proxy: self.proxy.as_deref(),
        };
        let (first, rest) = script.split_once('\n').unwrap_or((&script, ""));
        let head = self.scheduler.head(&submission);
        tokio::fs::write(&path, format!("{first}\n{head}{rest}")).await?;
        // The node reads the wrapper and the inputs as soon as the job starts.
        let synced = root.to_owned();
        tokio::task::spawn_blocking(move || sync_files(&synced))
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))??;
        // A submission is not cut short, as a job whose id is lost would run unwatched.
        let id = (self.scheduler.submit(&submission).await).map_err(|message| {
            machine::<S>(format!("{} did not take the job: {message}", S::NAME))
        })?;
        tracing::debug!(job = id, attempt = job.name, "submitted {} job", S::NAME);
        self.queue.add(&id).await;
        let ended = self.wait(&id, &control, cancel).await;
        match &ended {
            Ok(Ended::Done(_) | Ended::Stopped(_)) => self.settle(&id).await,
            _ => self.stop(&id).await,
        }
        self.queue.remove(&id).await;
        let ended = ended?;
        self.scheduler.ended(root)?;
        if engine && read_number(&engine_marker(root)).await == Some(1) {
            tracing::warn!(
                job = id,
                "the node did not confirm the removal of the container"
            );
        }
        self.finish(ended, root.to_owned(), job).await
    }

    /// Waits for the record of the container, or for the scheduler to tell that the job ended
    /// or is held.
    async fn wait(
        &self,
        id: &str,
        control: &Path,
        cancel: &CancellationToken,
    ) -> Result<Ended, ExecutorError> {
        let mut poll = Poll::new(self.poll);
        let mut ended: Option<Option<String>> = None;
        loop {
            poll.read(cancel, std::future::ready(()))
                .await
                .map_err(|_| ExecutorError::Cancelled)?;
            if let Some(status) = read_number(&control.join(CONTAINER_EXIT)).await {
                return Ok(Ended::Done(status));
            }
            if let Some(state) = &ended {
                if poll.troubled(true, EXIT_GRACE) {
                    return Ok(Ended::Stopped(state.clone()));
                }
                continue;
            }
            let look = tokio::select! {
                look = self.queue.look(&self.scheduler, id, self.poll) => look,
                () = cancel.cancelled() => return Err(ExecutorError::Cancelled),
            };
            match look {
                Look::Unknown | Look::Known(State::Active) => {}
                Look::Known(State::Held(reason)) => return Ok(Ended::Held(reason)),
                Look::Known(State::Ended(state)) => ended = Some(Some(state)),
                Look::Gone => ended = Some(None),
            }
            if ended.is_some() {
                // A shared file system may show the record of the container late.
                poll.troubled(true, EXIT_GRACE);
            }
        }
    }

