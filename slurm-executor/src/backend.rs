//! Runs an attempt of a task as one SLURM batch job: the inputs are placed in the scratch
//! directory of the attempt, and the job runs the wrapper in the task's container on a node.
//! So the nodes must see the scratch directory and the store at the same paths, as on a
//! cluster with a shared file system.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use executor::manifest::parse;
use executor::registry::{ImageConfig, ImageConfigs};
use executor::wrapper::{MANIFEST, STDERR};
use executor::{
    Access, Backend, CONTROL, ContainerRun, ExecutorError, Finished, Job, containers, place,
    registry,
};
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::images::{Images, file_name, pull_script, tag_file_name};
use crate::queue::{Look, Queue};
use crate::script::{
    CONTAINER_EXIT, Container, LOG, PullResources, SCRIPT, TIMED_OUT, job_script,
    job_script_of_containers, pull_options, sbatch_options,
};

/// How often the queue is asked about the jobs, by default.
const POLL: Duration = Duration::from_secs(10);

/// How long a job that ended may take to show the record of its container, which a shared
/// file system may show late.
const EXIT_GRACE: Duration = Duration::from_secs(30);

/// How long to wait after the first failed pull of an image.
const PULL_PAUSE: Duration = Duration::from_secs(20);

/// How often the job that pulls an image is tried before the tasks of the image fail.
const PULL_TRIES: u32 = 3;

/// How long a cancelled job may take to leave the queue before the backend stops waiting.
const CANCEL_GRACE: Duration = Duration::from_secs(60);

/// The tools of SLURM that the backend runs.
pub const TOOLS: [&str; 4] = ["sbatch", "squeue", "sacct", "scancel"];

/// Whether a program can run: a path to an executable file, or a name that the PATH has.
pub fn executable(program: &Path) -> bool {
    let runs = |path: &Path| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    };
    if program.components().count() > 1 {
        return runs(program);
    }
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|folder| runs(&folder.join(program))))
}

/// The SLURM executor's backend.
#[derive(Debug)]
pub struct SlurmBackend {
    /// Where each attempt gets its scratch directory, which the nodes must see at the same
    /// path.
    pub scratch: PathBuf,
    pub container: Container,
    /// More options of `sbatch`, like `--account=lab`.
    pub options: Vec<String>,
    /// The folder of `sbatch`, `squeue`, `sacct` and `scancel`, when they are not on the PATH.
    pub commands: Option<PathBuf>,
    /// How often the queue is asked about the jobs.
    pub poll: Duration,
    /// Where Apptainer and Singularity keep the SIF file of each image of a registry or address,
    /// which the nodes must see at the same path.
    pub images: PathBuf,
    /// What the job that pulls an image asks for.
    pub pull: PullResources,
    /// How long to wait after a failed pull before the next try; the wait grows with each
    /// try.
    pub pull_pause: Duration,
    /// The web proxy of the jobs, when the nodes reach the web only through one. `sbatch`
    /// hands its environment to the job, so the jobs get it as `HTTPS_PROXY` and
    /// `HTTP_PROXY` (and in lower case), and Apptainer pulls images through it.
    pub proxy: Option<String>,
    /// The entrypoints and default commands of images of registries, from the registries.
    pub configs: ImageConfigs,
    queue: Queue,
    made: Images,
    /// The nodes where a job of this run left no log, as they do not see the scratch folder.
    excluded: Mutex<BTreeSet<String>>,
}

/// An image config from the file that [`SlurmBackend::image_config`] keeps.
fn config_of(text: &str) -> Option<ImageConfig> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let words = |key: &str| -> Option<Vec<String>> {
        let list = value.get(key)?.as_array()?;
        list.iter()
            .map(|word| word.as_str().map(str::to_owned))
            .collect()
    };
    Some(ImageConfig {
        entrypoint: words("entrypoint")?,
        cmd: words("cmd")?,
        user: value.get("user")?.as_str()?.to_owned(),
    })
}

/// How a job ended.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// The job script recorded the exit code of the container.
    Done(i32),
    /// The job ended without that record, in this state when SLURM tells it.
    Stopped(Option<String>),
}

impl SlurmBackend {
    /// A backend with its attempts in `scratch`, whose jobs run their tasks with `container`.
    pub fn new(scratch: PathBuf, container: Container) -> SlurmBackend {
        SlurmBackend {
            images: scratch.join("images"),
            pull: PullResources::default(),
            pull_pause: PULL_PAUSE,
            proxy: None,
            configs: ImageConfigs::default(),
            scratch,
            container,
            options: Vec::new(),
            commands: None,
            poll: POLL,
            queue: Queue::default(),
            made: Images::default(),
            excluded: Mutex::default(),
        }
    }

    /// The tools of SLURM that are not there: not in the folder of `commands`, or not on the
    /// PATH. Runs need `sbatch`, `squeue` and `scancel`; `sacct` only tells how a job ended
    /// that SLURM has forgotten.
    pub fn missing_tools(&self) -> Vec<&'static str> {
        TOOLS
            .into_iter()
            .filter(|tool| !executable(&self.command(tool)))
            .collect()
    }

    fn command(&self, name: &str) -> PathBuf {
        match &self.commands {
            Some(folder) => folder.join(name),
            None => PathBuf::from(name),
        }
    }
}

impl Backend for SlurmBackend {
    async fn pin(&self, image: &str) -> Result<Option<String>, ExecutorError> {
        registry::pin(image).await
    }

    async fn run(
        &self,
        job: Job<'_>,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        let root = self.scratch.join(job.name);
        let placed = {
            let (root, staging) = (root.clone(), job.staging.clone());
            tokio::task::spawn_blocking(move || place(&root, &staging))
                .await
                .map_err(|error| std::io::Error::other(error.to_string()))?
        };
        let result = match placed {
            // Another attempt has this name; its directory is not ours to remove.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(error.into());
            }
            Err(error) => Err(error.into()),
            Ok(mounts) => self.attempt(job, &root, &mounts, cancel).await,
        };
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&root).await;
        }
        result
    }
}

impl SlurmBackend {
    /// The command that a container of `image` runs for these arguments. An image of a
    /// registry runs them through its entrypoint, or its default command when there are none,
    /// as `docker run`, the local executor and Kubernetes run them; a task that replaces the
    /// entrypoint runs them as they are. A SIF file or an address runs them as they are, as
    /// Apptainer's `exec` does.
    async fn task_command(
        &self,
        image: &task_model::Image,
        replaces_entrypoint: bool,
        arguments: &[String],
    ) -> Result<Vec<String>, ExecutorError> {
        let reference = image.reference.as_str();
        let file = reference.starts_with('/') || reference.ends_with(".sif");
        let registry = !file && !reference.contains("://");
        let command = match registry && !(replaces_entrypoint && !arguments.is_empty()) {
            true => self.image_config(reference).await?.command(arguments),
            false => arguments.to_vec(),
        };
        if command.is_empty() {
            return Err(ExecutorError::NoCommand(reference.to_owned()));
        }
        Ok(command)
    }

    /// The config of an image of a registry. An image pinned to its digest never changes, so
    /// its config is kept in a file next to its SIF file and asked of the registry only once:
    /// Docker Hub counts each request for a manifest against a limit per address, which all
    /// nodes of a cluster may share.
    async fn image_config(&self, reference: &str) -> Result<ImageConfig, ExecutorError> {
        let kept =
            file_name(reference).map(|name| self.images.join(name.replace(".sif", ".config.json")));
        if let Some(kept) = &kept
            && let Ok(text) = tokio::fs::read_to_string(kept).await
            && let Some(config) = config_of(&text)
        {
            return Ok(config);
        }
        let config = self.configs.get(reference).await?;
        if let Some(kept) = &kept {
            let text = serde_json::json!({
                "entrypoint": config.entrypoint,
                "cmd": config.cmd,
                "user": config.user,
            });
            // Written aside and moved, so that no other run reads half a file.
            let partial = kept.with_extension(format!("{}.partial", std::process::id()));
            let written = async {
                tokio::fs::create_dir_all(&self.images).await?;
                tokio::fs::write(&partial, text.to_string()).await?;
                tokio::fs::rename(&partial, kept).await
            };
            if written.await.is_err() {
                let _ = tokio::fs::remove_file(&partial).await;
            }
        }
        Ok(config)
    }

    /// Writes the wrapper and the job script of a staged attempt, submits the job and waits
    /// for its end.
    async fn attempt(
        &self,
        job: Job<'_>,
        root: &Path,
        mounts: &[executor::process::Mount],
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        let image = job.spec.image.as_ref().ok_or(ExecutorError::NoImage)?;
        let mut spec = job.spec.clone();
        let replaces = spec.replaces_entrypoint;
        spec.arguments = self.task_command(image, replaces, &spec.arguments).await?;
        for container in &mut spec.containers {
            let image = container.image.as_ref().ok_or(ExecutorError::NoImage)?;
            let replaces = container.replaces_entrypoint;
            container.arguments = self
                .task_command(image, replaces, &container.arguments)
                .await?;
        }
        let spec = &spec;
        let image = &self.runnable(image, cancel).await?;
        let control = root.join(CONTROL);
        let owner = tokio::fs::metadata(root).await?;
        let access = match &self.container {
            // Apptainer runs every container as the user who runs iunctus.
            Container::Apptainer(_) => Access {
                run_as: spec.run_as,
                ..Access::default()
            },
            // The image's user is not known here; it is taken as root.
            Container::Engine(engine) => {
                Access::of((owner.uid(), owner.gid()), engine.rootless, spec.run_as, "")
            }
        };
        if access.hand_back.is_some() {
            executor::access::open(root)?;
        }
        let name = format!("iunctus-{}", job.name);
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
        let script = match spec.containers.is_empty() {
            true => {
                tokio::fs::write(run.script(), run.wrapper(spec)).await?;
                job_script(&self.container, &run, gpus, job.placement.time_seconds)
            }
            // A task of several containers starts a container for each image, all at once.
            false => {
                let home = root.join(executor::HOME).to_string_lossy().into_owned();
                let owner = access.owner();
                let folders = executor::wrapper::Folders {
                    work: &run.inside(),
                    control: &control.to_string_lossy(),
                    home: &home,
                    outbox: "",
                    owner: &owner,
                };
                let groups = containers::write_wrappers(spec, &control, &folders)?;
                // The folders of the wrappers are new, so they are opened too.
                if access.hand_back.is_some() {
                    executor::access::open(root)?;
                }
                let mut images = Vec::new();
                for (group, _) in &groups {
                    images.push(self.runnable(&group.image, cancel).await?);
                }
                let names: Vec<String> = (0..groups.len())
                    .map(|index| format!("{name}-{index}"))
                    .collect();
                let paths: Vec<PathBuf> = groups.iter().map(|(_, path)| path.into()).collect();
                let runs: Vec<ContainerRun<'_>> = (groups.iter().enumerate())
                    .map(|(index, _)| ContainerRun {
                        name: &names[index],
                        image: &images[index],
                        wrapper_path: Some(&paths[index]),
                        ..run
                    })
                    .collect();
                let exits: Vec<Vec<PathBuf>> = (groups.iter())
                    .map(|(group, _)| {
                        (group.members.iter())
                            .map(|&position| control.join(containers::exit_file(spec, position)))
                            .collect()
                    })
                    .collect();
                let time = job.placement.time_seconds;
                job_script_of_containers(&self.container, &runs, &exits, gpus, time)
            }
        };
        tokio::fs::write(control.join(SCRIPT), script).await?;
        let options = self
            .with_excluded(sbatch_options(job, root, &self.options))
            .await;
        let id = self.submit(options, &control.join(SCRIPT)).await?;
        self.queue.add(&id).await;
        let ended = self.wait(&id, &control, cancel).await;
        self.queue.remove(&id).await;
        let ended = ended?;
        // A job that left no log at all ran on a node that does not see the scratch folder.
        let lost = !tokio::fs::try_exists(control.join(LOG))
            .await
            .unwrap_or(true);
        let node = match lost {
            true => self.leave_out(&id).await,
            false => None,
        };
        let time = job.placement.time_seconds;
        finish(ended, root.to_owned(), time, job.spec)
            .await
            .map_err(|error| match (error, node) {
                (ExecutorError::Engine { engine, message }, Some(node)) => ExecutorError::Engine {
                    engine,
                    message: format!(
                        "{message} It ran on the node {node}, which the later jobs of this run \
                         leave out."
                    ),
                },
                (error, _) => error,
            })
    }

    /// Asks SLURM for the node of a job, and leaves that node out of the later jobs.
    async fn leave_out(&self, id: &str) -> Option<String> {
        let node = self
            .queue
            .node(&self.command("squeue"), &self.command("sacct"), id)
            .await?;
        self.excluded.lock().await.insert(node.clone());
        Some(node)
    }

    /// The options of a job with `--exclude` for the nodes that failed jobs of this run,
    /// before the options of the user, which come last and so win.
    async fn with_excluded(&self, mut options: Vec<String>) -> Vec<String> {
        let excluded = self.excluded.lock().await;
        if !excluded.is_empty() {
            let nodes: Vec<&str> = excluded.iter().map(String::as_str).collect();
            let at = options.len().saturating_sub(self.options.len());
            options.insert(at, format!("--exclude={}", nodes.join(",")));
        }
        options
    }

    /// The image as the jobs run it: with Apptainer or Singularity, the SIF file of the image,
    /// pulled once by a job of its own, else the image itself. The file of an image pinned to
    /// its digest, or of an address, serves later runs too; that of a tag is pulled once per
    /// run, as the tag may move.
    async fn runnable(
        &self,
        image: &task_model::Image,
        cancel: &CancellationToken,
    ) -> Result<task_model::Image, ExecutorError> {
        let Container::Apptainer(program) = &self.container else {
            return Ok(image.clone());
        };
        let reference = &image.reference;
        let file = match (file_name(reference), tag_file_name(reference)) {
            (Some(file), _) => Some((file, true)),
            (None, Some(file)) => Some((file, false)),
            (None, None) => None,
        };
        match file {
            Some((file, lasting)) => {
                let path = self.sif(program, reference, &file, lasting, cancel).await?;
                Ok(task_model::Image {
                    reference: path.to_string_lossy().into_owned(),
                    platform: image.platform.clone(),
                })
            }
            None => Ok(image.clone()),
        }
    }

    /// The SIF file of an image, pulled once by a job of its own. A file that is already there
    /// counts only when it is `lasting`. The tasks of the image share one pull per run, also
    /// when it fails: a failed pull is tried again a few times, with a growing pause, and then
    /// every task of the image fails with the same error.
    async fn sif(
        &self,
        program: &str,
        reference: &str,
        file: &str,
        lasting: bool,
        cancel: &CancellationToken,
    ) -> Result<PathBuf, ExecutorError> {
        let path = self.images.join(file);
        let cell = self.made.cell(reference).await;
        let made = cell.get_or_try_init(|| async {
            if lasting && tokio::fs::try_exists(&path).await.unwrap_or(false) {
                return Ok(Ok(path.clone()));
            }
            let mut tries = 0;
            loop {
                tries += 1;
                let reason = match self.pull(program, reference, &path, cancel).await {
                    Ok(path) => return Ok(Ok(path)),
                    // A cancelled task leaves the pull to the next task of the image.
                    Err(ExecutorError::Cancelled) => return Err(ExecutorError::Cancelled),
                    Err(ExecutorError::Engine { message, .. }) => message,
                    Err(error) => error.to_string(),
                };
                if tries >= PULL_TRIES {
                    return Ok(Err(format!("{reason}; the pull was tried {tries} times")));
                }
                tokio::select! {
                    () = tokio::time::sleep(self.pull_pause * tries) => {}
                    () = cancel.cancelled() => return Err(ExecutorError::Cancelled),
                }
            }
        });
        match made.await? {
            Ok(path) => Ok(path.clone()),
            Err(reason) => Err(ExecutorError::MissingImage {
                image: reference.to_owned(),
                reason: reason.clone(),
            }),
        }
    }

    /// Submits the job that pulls an image into its SIF file at `path`, and waits for it.
    async fn pull(
        &self,
        program: &str,
        reference: &str,
        path: &Path,
        cancel: &CancellationToken,
    ) -> Result<PathBuf, ExecutorError> {
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        let name = format!(
            "image-{}-{}",
            &stem[..stem.len().min(19)],
            std::process::id()
        );
        let root = self.images.join(&name);
        let control = root.join(CONTROL);
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&control).await?;
        let script = pull_script(program, reference, path, &control.join(CONTAINER_EXIT));
        tokio::fs::write(control.join(SCRIPT), script).await?;
        let options = self
            .with_excluded(pull_options(&name, &root, &self.pull, &self.options))
            .await;
        let id = self.submit(options, &control.join(SCRIPT)).await?;
        self.queue.add(&id).await;
        let ended = self.wait(&id, &control, cancel).await;
        self.queue.remove(&id).await;
        let pulled = match ended? {
            Ended::Done(0) if tokio::fs::try_exists(path).await.unwrap_or(false) => {
                Ok(path.to_owned())
            }
            Ended::Done(status) => Err(format!(
                "{program} could not pull the image {reference} (exit code {status})"
            )),
            Ended::Stopped(state) => Err(format!(
                "the job that pulls the image {reference} ended as {}",
                state.as_deref().unwrap_or("unknown")
            )),
        };
        let pulled = match pulled {
            Ok(path) => Ok(path),
            Err(message) => Err(machine(explain(message, &root).await)),
        };
        let _ = tokio::fs::remove_dir_all(&root).await;
        pulled
    }

    /// Submits a job with its options and script, and gives its id.
    async fn submit(&self, options: Vec<String>, script: &Path) -> Result<String, ExecutorError> {
        let mut sbatch = Command::new(self.command("sbatch"));
        if let Some(proxy) = &self.proxy {
            for name in ["HTTPS_PROXY", "HTTP_PROXY", "https_proxy", "http_proxy"] {
                sbatch.env(name, proxy);
            }
        }
        let output = sbatch
            .args(options)
            .arg(script)
            .output()
            .await
            .map_err(|error| machine(format!("cannot run sbatch: {error}")))?;
        let text = String::from_utf8_lossy(&output.stdout);
        // `--parsable` gives `<id>` or `<id>;<cluster>`.
        let id = text.trim().split(';').next().unwrap_or_default();
        if !output.status.success() || id.is_empty() {
            let error = String::from_utf8_lossy(&output.stderr);
            return Err(machine(format!(
                "sbatch did not take the job: {}",
                error.trim()
            )));
        }
        Ok(id.to_owned())
    }

    /// Waits for a job to end, and cancels it when `cancel` fires.
    async fn wait(
        &self,
        id: &str,
        control: &Path,
        cancel: &CancellationToken,
    ) -> Result<Ended, ExecutorError> {
        let squeue = self.command("squeue");
        let mut pause = Duration::from_millis(250);
        let mut ended: Option<(Instant, Option<String>)> = None;
        loop {
            tokio::select! {
                () = tokio::time::sleep(pause) => {}
                () = cancel.cancelled() => {
                    self.cancel(id).await;
                    return Err(ExecutorError::Cancelled);
                }
            }
            pause = (pause * 2).min(self.poll);
            if let Some(status) = read_number(&control.join(CONTAINER_EXIT)).await {
                return Ok(Ended::Done(status));
            }
            if let Some((since, state)) = &ended {
                if since.elapsed() >= EXIT_GRACE {
                    return Ok(Ended::Stopped(state.clone()));
                }
                continue;
            }
            let state = match self.queue.look(id, self.poll, &squeue).await {
                Look::Active | Look::Unknown => continue,
                Look::Ended(state) => Some(state),
                Look::Gone => self.queue.accounted(&self.command("sacct"), id).await,
            };
            // A job that ended well has recorded its container; the shared file system may
            // show the record late. Other ends need no wait.
            match state.as_deref() {
                Some("COMPLETED") | None => ended = Some((Instant::now(), state)),
                Some(_) => return Ok(Ended::Stopped(state)),
            }
        }
    }

    /// Cancels a job and waits for it to leave the queue, so that its container is gone.
    async fn cancel(&self, id: &str) {
        let _ = Command::new(self.command("scancel")).arg(id).output().await;
        let squeue = self.command("squeue");
        let deadline = Instant::now() + CANCEL_GRACE;
        while Instant::now() < deadline {
            tokio::time::sleep(self.poll.min(Duration::from_secs(1))).await;
            if matches!(
                self.queue.look(id, self.poll, &squeue).await,
                Look::Ended(_) | Look::Gone
            ) {
                break;
            }
        }
    }
}

/// What an attempt left behind, by how its job ended.
async fn finish(
    ended: Ended,
    root: PathBuf,
    time: Option<u64>,
    spec: &task_model::TaskSpec,
) -> Result<Finished, ExecutorError> {
    let control = root.join(CONTROL);
    let recorded = containers::recorded(spec, &control);
    let (exit_code, note) = match outcome(recorded, &ended, time) {
        Ok(outcome) => outcome,
        Err(message) => return Err(machine(explain(message, &root).await)),
    };
    if let Some(note) = note {
        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(control.join(STDERR))
            .await?;
        file.write_all(note.as_bytes()).await?;
    }
    let files = match exit_code {
        0 => {
            let manifest = tokio::fs::read_to_string(control.join(MANIFEST)).await;
            parse(&manifest.unwrap_or_default())?
        }
        _ => Vec::new(),
    };
    Ok(Finished {
        exit_code: Some(exit_code),
        files,
        usage: executor::Usage::of(&root),
        root,
    })
}

/// The exit code of the task and a note for its standard error, from the exit code that the
/// wrapper recorded and the end of the job; `Err` when the machine failed rather than the task.
///
/// The wrapper records the tool's exit code and ends with 0. A container stopped by a signal,
/// as for lack of memory, ends the task with its own exit code. A job that ran out of time or
/// memory ends the task as a stopped container would. Other ends are failures of the machine,
/// like a container runtime that could not start the image, or a node that failed.
fn outcome(
    recorded: Option<i32>,
    ended: &Ended,
    time: Option<u64>,
) -> Result<(i32, Option<String>), String> {
    match ended {
        Ended::Done(0) => recorded
            .map(|code| (code, None))
            .ok_or_else(|| "the container ended without the exit code of the task".to_owned()),
        Ended::Done(TIMED_OUT) => Ok((143, Some(too_long(time)))),
        Ended::Done(status @ 129..=192) => Ok((
            *status,
            Some(format!(
                "iunctus: the container ended with exit code {status}, maybe for lack of memory\n"
            )),
        )),
        Ended::Done(status) => Err(format!(
            "the container could not run the task (exit code {status})"
        )),
        Ended::Stopped(Some(state)) if state == "TIMEOUT" => Ok((143, Some(too_long(time)))),
        Ended::Stopped(Some(state)) if state == "OUT_OF_MEMORY" => Ok((
            137,
            Some("iunctus: SLURM stopped the task for lack of memory\n".to_owned()),
        )),
        Ended::Stopped(Some(state)) => Err(format!("the job ended as {state}")),
        Ended::Stopped(None) => Err("the job ended without a record of its container".into()),
    }
}

/// The note of a task that ran longer than its time, or than SLURM allows.
fn too_long(time: Option<u64>) -> String {
    match time {
        Some(time) => format!("iunctus: the task ran longer than its time of {time} seconds\n"),
        None => "iunctus: the task ran longer than SLURM allows\n".to_owned(),
    }
}

/// A failure of the machine with the end of the job's log, or a hint when the job left none.
async fn explain(message: String, root: &Path) -> String {
    let log = tokio::fs::read_to_string(root.join(CONTROL).join(LOG)).await;
    let log = log.unwrap_or_default();
    let lines: Vec<&str> = log.lines().filter(|line| !line.trim().is_empty()).collect();
    match lines.len() {
        0 => format!(
            "{message}, and left no log; do the nodes see {} at the same path?",
            root.display()
        ),
        count => format!("{message}: {}", lines[count.saturating_sub(5)..].join("; ")),
    }
}

async fn read_number(path: &Path) -> Option<i32> {
    let text = tokio::fs::read_to_string(path).await.ok()?;
    text.trim().parse().ok()
}

fn machine(message: String) -> ExecutorError {
    ExecutorError::Engine {
        engine: "SLURM".into(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_end_tasks_or_fail_the_machine() {
        let done = |status| Ended::Done(status);
        let stopped = |state: &str| Ended::Stopped(Some(state.to_owned()));
        assert_eq!(outcome(Some(3), &done(0), None), Ok((3, None)));
        assert!(outcome(None, &done(0), None).is_err());
        assert_eq!(
            outcome(None, &done(137), None).map(|(code, _)| code),
            Ok(137)
        );
        assert!(outcome(None, &done(255), None).is_err());
        assert!(outcome(Some(0), &done(125), None).is_err());
        let timed_out = outcome(Some(0), &done(124), Some(3)).expect("an end");
        assert_eq!(timed_out.0, 143);
        assert!(timed_out.1.expect("a note").contains("3 seconds"));
        let timeout = outcome(Some(0), &stopped("TIMEOUT"), Some(60)).expect("an end");
        assert_eq!(timeout.0, 143);
        assert!(timeout.1.expect("a note").contains("60 seconds"));
        let memory = outcome(None, &stopped("OUT_OF_MEMORY"), None);
        assert_eq!(memory.map(|(code, _)| code), Ok(137));
        for state in ["NODE_FAIL", "PREEMPTED", "BOOT_FAIL", "CANCELLED", "FAILED"] {
            assert!(outcome(None, &stopped(state), None).is_err(), "{state}");
        }
        assert!(outcome(None, &Ended::Stopped(None), None).is_err());
    }
}
