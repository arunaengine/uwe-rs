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
    Access, Backend, CONTROL, ContainerRun, ExecutorError, Finished, Job, Staging, containers,
    place,
};
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::backend::{explain, read_number, record, sync_files};
use crate::remote::{Helper, Links, NodeStaging, records};
use crate::script::{
    CONTAINER_EXIT, Container, ENGINE_EXIT, NOT_SEEN, SCRIPT, TIMED_OUT, check_binds, check_gpus,
    engine_marker, job_script, shared_memory, size_memory,
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

    /// The file of the job's output in the control folder of an attempt, like `pbs.log`.
    const LOG: &'static str;

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
/// folder at the same path or that get their files through a staging root.
#[derive(Debug)]
pub struct GridBackend<S> {
    pub scheduler: S,
    /// Where each attempt gets its folder.
    pub scratch: PathBuf,
    pub container: Container,
    /// How the jobs get their files when the nodes share no folders with this machine.
    pub remote: Option<NodeStaging>,
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

/// Where an attempt keeps its objects in the staging root.
struct Staged {
    prefix: String,
    plan: String,
}

impl<S: Scheduler> GridBackend<S> {
    /// A backend whose attempts get their folders in `scratch` and run their tasks with
    /// `container`.
    pub fn new(scheduler: S, scratch: PathBuf, container: Container) -> GridBackend<S> {
        GridBackend {
            scheduler,
            scratch,
            container,
            remote: None,
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
        // Without shared folders the inputs go to the node through the staging root.
        let staging = match self.remote {
            Some(_) => Staging::default(),
            None => job.staging.clone(),
        };
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
        let mut staged = None;
        let result = self.attempt(job, &root, &mounts, &mut staged, cancel).await;
        if let (Some(remote), Some(staged)) = (&self.remote, &staged) {
            remote.stage.clean(&staged.prefix, &staged.plan).await;
        }
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
    /// for its end. A plan in the staging root goes to `staged` as soon as it is there.
    async fn attempt(
        &self,
        job: Job<'_>,
        root: &Path,
        mounts: &[executor::process::Mount],
        staged: &mut Option<Staged>,
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
        if engine && self.remote.is_none() {
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
        let script = match &self.remote {
            Some(remote) => self.node_script(job, spec, run, remote, staged).await?,
            None => {
                let wrapper = run.wrapper(spec);
                tokio::fs::write(run.script(), &wrapper).await?;
                job_script(&self.container, &run, gpus, time, wrapper.len() as u64)
            }
        };
        let script = size_memory(&self.container, script, shm);
        let path = control.join(SCRIPT);
        let submission = Submission {
            job,
            root,
            script: &path,
            remote: self.remote.is_some(),
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
        let prefix = staged.as_ref().map(|staged| staged.prefix.as_str());
        let ended = self.wait(&id, &control, prefix, cancel).await;
        match &ended {
            Ok(Ended::Done(_) | Ended::Stopped(_)) => self.settle(&id).await,
            _ => self.stop(&id).await,
        }
        self.queue.remove(&id).await;
        let ended = ended?;
        if let (Some(remote), Some(prefix)) = (&self.remote, prefix) {
            let names = records(S::LOG);
            executor::remote::patiently(|| remote.stage.collect(prefix, root, &names)).await?;
            if matches!(ended, Ended::Done(0))
                && read_number(&control.join(CONTAINER_EXIT)).await.is_none()
            {
                return Err(machine::<S>(
                    "the staged completion record is missing".to_owned(),
                ));
            }
        }
        self.scheduler.ended(root)?;
        if engine && self.remote.is_none() && read_number(&engine_marker(root)).await == Some(1) {
            tracing::warn!(
                job = id,
                "the node did not confirm the removal of the container"
            );
        }
        self.finish(ended, root.to_owned(), job).await
    }

    /// The job script of an attempt on a node without the shared folders: the files go through
    /// the staging root, between stage-in and stage-out by the uwe helper.
    async fn node_script(
        &self,
        job: Job<'_>,
        spec: &task_model::TaskSpec,
        run: ContainerRun<'_>,
        remote: &NodeStaging,
        staged: &mut Option<Staged>,
    ) -> Result<String, ExecutorError> {
        let text = |path: PathBuf| path.to_string_lossy().into_owned();
        let root = run.root;
        let (control, home) = (text(root.join(CONTROL)), text(root.join(executor::HOME)));
        let (outbox, owner) = (text(root.join("outbox")), run.access.owner());
        let folders = executor::wrapper::Folders {
            work: &run.inside(),
            control: &control,
            home: &home,
            outbox: &outbox,
            owner: &owner,
        };
        let transfer = text(root.join(executor::helper::TRANSFER));
        let wrapper = executor::wrapper::wrapper(spec, &folders);
        let wrapper = executor::capture::capture(&wrapper, &transfer)?;
        let extra = [("control/wrapper.sh", wrapper)];
        let binary = match &remote.helper {
            Helper::Upload(binary) => Some(remote.stage.binary(binary).await?),
            _ => None,
        };
        let open = run.access.hand_back.is_some();
        let planned = (remote.stage)
            .plan(job, root, &extra, &[], &records(S::LOG), open)
            .await?;
        *staged = Some(Staged {
            prefix: planned.prefix.clone(),
            plan: planned.plan.clone(),
        });
        let run = ContainerRun {
            mounts: &planned.mounts,
            ..run
        };
        check_binds(&self.container, &run)?;
        // Without it, a job whose helper cannot start keeps its log on the node.
        let log = match remote.stage.record(&planned.prefix, S::LOG).await {
            Ok(log) => Some(log),
            Err(error) => {
                tracing::warn!(%error, "jobs cannot send their logs without the helper");
                None
            }
        };
        let engine = match &self.container {
            Container::Engine(_) => {
                (remote.stage)
                    .write_record(&planned.prefix, ENGINE_EXIT, b"0\n")
                    .await?;
                Some(remote.stage.record(&planned.prefix, ENGINE_EXIT).await?)
            }
            Container::Apptainer(_) => None,
        };
        let links = Links {
            plan: &planned.location,
            binary: binary.as_deref(),
            log: log.as_ref(),
            engine: engine.as_ref(),
        };
        let gpus = job.placement.accelerators.iter().any(|gpu| gpu.count > 0);
        let time = job.placement.time_seconds;
        Ok(crate::remote::job_script(
            &self.container,
            &run,
            remote,
            gpus,
            time,
            links,
            S::LOG,
        ))
    }

    /// Waits for the record of the container, or for the scheduler to tell that the job ended
    /// or is held. A job that sends its records to `prefix` of the staging root shows its end
    /// there.
    async fn wait(
        &self,
        id: &str,
        control: &Path,
        prefix: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<Ended, ExecutorError> {
        let mut poll = Poll::new(self.poll);
        let mut ended: Option<Option<String>> = None;
        loop {
            poll.read(cancel, std::future::ready(()))
                .await
                .map_err(|_| ExecutorError::Cancelled)?;
            if let (Some(remote), Some(prefix), Some(root)) =
                (&self.remote, prefix, control.parent())
                && let Err(error) = remote.stage.fetch(prefix, CONTAINER_EXIT, root).await
            {
                // The staging root may fail for a moment; the job goes on meanwhile.
                tracing::debug!(job = id, %error, "cannot look at the staging root");
            }
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

    /// Waits a while for a job whose container ended to leave the queue, so that the
    /// scheduler has written its log.
    async fn settle(&self, id: &str) {
        let pause = self.poll.min(Duration::from_secs(1));
        let left = executor::poll::until_stopped(END_GRACE, pause, || async {
            let look = self.queue.look(&self.scheduler, id, pause).await;
            (!matches!(look, Look::Unknown | Look::Known(State::Active))).then_some(())
        })
        .await;
        if left.is_err() {
            tracing::warn!(
                job = id,
                "the {} job stays in the queue after its end",
                S::NAME
            );
        }
    }

    /// Removes a job that is cancelled, held or failed, and waits a while for it to go.
    async fn stop(&self, id: &str) {
        if let Err(error) = self.scheduler.remove(id).await {
            tracing::warn!(job = id, error, "cannot remove the {} job", S::NAME);
        }
        let pause = self.poll.min(Duration::from_secs(1));
        let gone = executor::poll::until_stopped(CANCEL_GRACE, pause, || async {
            let look = self.queue.look(&self.scheduler, id, pause).await;
            matches!(look, Look::Gone | Look::Known(State::Ended(_))).then_some(())
        })
        .await;
        if gone.is_err() {
            tracing::warn!(
                job = id,
                "the removal of the {} job was not confirmed",
                S::NAME
            );
        }
    }

    /// What an attempt left behind, by how its job ended.
    async fn finish(
        &self,
        ended: Ended,
        root: PathBuf,
        job: Job<'_>,
    ) -> Result<Finished, ExecutorError> {
        let control = root.join(CONTROL);
        let shared = self.remote.is_none();
        let recorded = match ended {
            Ended::Done(0) => record(job.spec, &control).await,
            _ => containers::recorded(job.spec, &control),
        };
        let time = job.placement.time_seconds;
        let (exit_code, note) = match outcome::<S>(recorded, &ended, time) {
            Ok(outcome) => outcome,
            Err(message) => {
                return Err(machine::<S>(explain(message, &root, S::LOG, shared).await));
            }
        };
        if let Some(note) = note {
            use tokio::io::AsyncWriteExt;
            let path = control.join(STDERR);
            let mut file = (tokio::fs::OpenOptions::new().create(true).append(true))
                .open(path)
                .await?;
            file.write_all(note.as_bytes()).await?;
            file.flush().await?;
        }
        let files = match exit_code {
            0 => parse(
                &tokio::fs::read_to_string(control.join(MANIFEST))
                    .await
                    .unwrap_or_default(),
            )?,
            _ => Vec::new(),
        };
        let mut finished = Finished {
            address: None,
            exit_code: Some(exit_code),
            files,
            usage: executor::Usage::of(&root),
            root,
        };
        if shared {
            finished.collect_outputs(job.spec, job.staging)?;
        }
        Ok(finished)
    }
}

/// The exit code of the task and a note for its standard error, from the exit code that the
/// wrapper recorded and the end of the job; `Err` when the machine failed rather than the task.
fn outcome<S: Scheduler>(
    recorded: Option<i32>,
    ended: &Ended,
    time: Option<u64>,
) -> Result<(i32, Option<String>), String> {
    match ended {
        Ended::Done(0) => recorded
            .map(|code| (code, None))
            .ok_or_else(|| "the container ended without the exit code of the task".to_owned()),
        Ended::Done(TIMED_OUT) => Ok((
            143,
            Some(format!(
                "uwe: the task ran longer than its time of {} seconds\n",
                time.unwrap_or_default()
            )),
        )),
        Ended::Done(NOT_SEEN) => Err("the job did not start the container".to_owned()),
        Ended::Done(status @ 129..=192) => Ok((
            *status,
            Some(format!(
                "uwe: the container ended with exit code {status}, maybe for lack of memory\n"
            )),
        )),
        // A captured wrapper ends with the code of its task, which it recorded first.
        Ended::Done(status) if recorded == Some(*status) => Ok((*status, None)),
        Ended::Done(status) => Err(format!(
            "the container could not run the task (exit code {status})"
        )),
        Ended::Held(reason) => Err(format!(
            "{} held the job ({reason}), so uwe removed it",
            S::NAME
        )),
        Ended::Stopped(Some(state)) => Err(format!(
            "the job ended as {state} without a record of its container"
        )),
        Ended::Stopped(None) => Err("the job ended without a record of its container".into()),
    }
}

fn machine<S: Scheduler>(message: String) -> ExecutorError {
    ExecutorError::Engine {
        engine: S::NAME.into(),
        message,
    }
}

/// Runs a tool of a scheduler, which may take [`TOOL_GRACE`], and gives its output.
pub async fn call(command: &mut Command) -> Result<std::process::Output, String> {
    let program = command
        .as_std()
        .get_program()
        .to_string_lossy()
        .into_owned();
    match tokio::time::timeout(TOOL_GRACE, command.kill_on_drop(true).output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(format!("cannot run {program}: {error}")),
        Err(_) => Err(format!("{program} did not answer within {TOOL_GRACE:?}")),
    }
}

/// What the queue tells of a job.
#[derive(Debug)]
enum Look {
    Known(State),
    /// The scheduler does not know the job.
    Gone,
    /// The scheduler could not be asked.
    Unknown,
}

/// The jobs of a backend and the last answer of the scheduler about all of them, so that one
/// call of its tool answers for every waiting attempt.
#[derive(Debug, Default)]
struct Queue {
    snapshot: Mutex<Snapshot>,
}

#[derive(Debug, Default)]
struct Snapshot {
    /// The jobs, by when they were submitted.
    jobs: HashMap<String, Instant>,
    states: HashMap<String, State>,
    /// When the scheduler answered, and whether it did.
    asked: Option<(Instant, bool)>,
}

impl Queue {
    async fn add(&self, id: &str) {
        let mut snapshot = self.snapshot.lock().await;
        snapshot.jobs.insert(id.to_owned(), Instant::now());
    }

    async fn remove(&self, id: &str) {
        let mut snapshot = self.snapshot.lock().await;
        snapshot.jobs.remove(id);
        snapshot.states.remove(id);
    }

    /// The state of a job, from an answer of the scheduler that is younger than `age` and than
    /// the submission of the job.
    async fn look<S: Scheduler>(&self, scheduler: &S, id: &str, age: Duration) -> Look {
        let mut snapshot = self.snapshot.lock().await;
        let since = snapshot.jobs.get(id).copied();
        let stale = match snapshot.asked {
            None => true,
            Some((at, _)) => at.elapsed() >= age || since.is_some_and(|since| since > at),
        };
        if stale {
            let ids: Vec<String> = snapshot.jobs.keys().cloned().collect();
            let answer = scheduler.states(&ids).await;
            let answered = answer.is_ok();
            match answer {
                Ok(states) => snapshot.states = states,
                Err(error) => tracing::debug!(error, "cannot ask {} about its jobs", S::NAME),
            }
            snapshot.asked = Some((Instant::now(), answered));
        }
        match (snapshot.states.get(id), snapshot.asked) {
            (_, Some((_, false))) => Look::Unknown,
            (Some(state), _) => Look::Known(state.clone()),
            (None, _) => Look::Gone,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Fake;

    impl Scheduler for Fake {
        const NAME: &'static str = "Fake";
        const LOG: &'static str = "fake.log";

        async fn submit(&self, _: &Submission<'_>) -> Result<String, String> {
            Ok("1".into())
        }

        async fn states(&self, ids: &[String]) -> Result<HashMap<String, State>, String> {
            match ids.iter().any(|id| id == "broken") {
                true => Err("no answer".into()),
                false => Ok(ids.iter().map(|id| (id.clone(), State::Active)).collect()),
            }
        }

        async fn remove(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn answers_new_jobs() {
        let queue = Queue::default();
        let age = Duration::from_secs(3600);
        assert!(matches!(queue.look(&Fake, "7", age).await, Look::Gone));
        // A job submitted after the last answer gets a fresh one.
        queue.add("7").await;
        assert!(matches!(
            queue.look(&Fake, "7", age).await,
            Look::Known(State::Active)
        ));
        queue.add("broken").await;
        assert!(matches!(
            queue.look(&Fake, "broken", age).await,
            Look::Unknown
        ));
    }

    #[test]
    fn classifies_job_ends() {
        let done = |status| Ended::Done(status);
        assert_eq!(outcome::<Fake>(Some(3), &done(0), None), Ok((3, None)));
        assert!(outcome::<Fake>(None, &done(0), None).is_err());
        let timed_out = outcome::<Fake>(Some(0), &done(TIMED_OUT), Some(3)).expect("an end");
        assert_eq!(timed_out.0, 143);
        assert!(timed_out.1.expect("a note").contains("3 seconds"));
        assert_eq!(outcome::<Fake>(Some(2), &done(2), None), Ok((2, None)));
        let held = outcome::<Fake>(None, &Ended::Held("no node".into()), None);
        assert_eq!(
            held,
            Err("Fake held the job (no node), so uwe removed it".into())
        );
        assert!(outcome::<Fake>(None, &Ended::Stopped(None), None).is_err());
    }
}
