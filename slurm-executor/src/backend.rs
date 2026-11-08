//! Runs staged task attempts as SLURM batch jobs and recovers their ownership.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;
use std::fs::{File, TryLockError};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use executor::manifest::parse;
use executor::poll::{CANCEL_GRACE, Poll};
use executor::registry::{ImageConfig, ImageConfigs};
use executor::wrapper::{MANIFEST, STDERR};
use executor::{
    Access, Backend, CONTROL, ContainerRun, ExecutorError, Finished, Job, Staging, containers,
    place, registry,
};
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::images::{
    Images, archive_config, archive_file, archive_path, file_name, image_id, pull_script,
    tag_file_name,
};
use crate::queue::{Look, Queue};
use crate::remote::{NodeStaging, records};
use crate::script::{
    CONTAINER_EXIT, Container, ENGINE_EXIT, LOG, NOT_SEEN, PullResources, SCRIPT, TIMED_OUT,
    container_script, job_script, per_cpu_memory, pull_options, sbatch_options,
};

/// How often the queue is asked about the jobs, by default.
const POLL: Duration = Duration::from_secs(10);

/// How long a job that ended may take to show the record of its container, and a container
/// that ended well the record of its task, which a shared file system may show late.
const EXIT_GRACE: Duration = Duration::from_secs(30);

/// How long `sbatch` may take, as a busy controller answers slowly. A submission whose answer is
/// lost is found again by its name and script.
const SUBMIT_GRACE: Duration = Duration::from_secs(60);

/// How much longer than its task a job may run, by default.
const STAGING_TIME: Duration = Duration::from_secs(5 * 60);

/// How long to wait after the first failed pull of an image.
const PULL_PAUSE: Duration = Duration::from_secs(20);

/// How often the job that pulls an image is tried before the tasks of the image fail.
const PULL_TRIES: u32 = 3;

/// The image archive of a pull job, in its folder.
const ARCHIVE: &str = "image.tar";

/// The image archive of an attempt on a node without shared folders, in its folder.
const ARCHIVE_PATH: &str = "control/image.tar";

/// The tools of SLURM that the backend runs.
pub const TOOLS: [&str; 4] = ["sbatch", "squeue", "sacct", "scancel"];

/// The variables of this process that `sbatch` and the jobs get: what tools and Apptainer
/// need to run, and the web proxy. Other variables, like credentials of the service, stay.
const PASSED: [&str; 16] = [
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "TZ",
    "TMPDIR",
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "https_proxy",
    "http_proxy",
    "all_proxy",
    "no_proxy",
];

/// The prefixes of the variables that configure SLURM and Apptainer, which pass too.
const PASSED_PREFIXES: [&str; 4] = ["SLURM_", "SBATCH_", "APPTAINER_", "SINGULARITY_"];

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
    /// How long `sbatch` may take before its submission counts as unclear.
    pub submit_grace: Duration,
    /// How long one call of `squeue`, `sacct` or `scancel` may take, for a busy controller.
    pub query_grace: Duration,
    /// Where Apptainer and Singularity keep the SIF file of each image of a registry or address,
    /// which the nodes must see at the same path.
    pub images: PathBuf,
    /// What the job that pulls an image asks for.
    pub pull: PullResources,
    /// How long to wait after a failed pull before the next try; the wait grows with each
    /// try.
    pub pull_pause: Duration,
    /// The web proxy of the jobs, when the nodes reach the web only through one. `sbatch`
    /// hands the variables it exports to the job, so the jobs get it as `HTTPS_PROXY` and
    /// `HTTP_PROXY` (and in lower case), and Apptainer pulls images through it.
    pub proxy: Option<String>,
    /// The entrypoints and default commands of images of registries, from the registries.
    pub configs: ImageConfigs,
    /// How the jobs get their files when the nodes share no folders with this machine; `None`
    /// when they see the scratch folder at the same path.
    pub remote: Option<NodeStaging>,
    /// How much longer than its task a job may run: for staging, the wait for its files and the
    /// start of its container. The task itself keeps its time.
    pub staging_time: Duration,
    /// Where the archives of images that tasks name by image ID, `sha256:<hex>`, are saved as
    /// `sha256-<hex>.tar`, like those of CWL tools.
    pub archives: Option<PathBuf>,
    /// For a cluster that gives each CPU a fixed memory and refuses memory requests: the GB
    /// per CPU. The jobs then ask for no memory but enough CPUs.
    pub cpu_memory_gb: Option<f64>,
    /// The id of the run, which the comment of each job and each journal names.
    pub run: String,
    queue: Queue,
    made: Images,
    recovery: Mutex<executor::history::Scan>,
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
    /// SLURM held the job for this reason and would not run it on its own; uwe cancels it.
    Held(String),
    /// The job asked for more time than its partition allows; uwe cancels it.
    OverTime,
}

/// What `sbatch` did with a job.
enum Submission {
    /// It took the job, which got this id.
    Taken(Scheduled),
    /// It refused the time of the job, as more than the partition allows.
    OverTime,
}

#[derive(Clone)]
pub(crate) struct Scheduled {
    pub(crate) id: String,
    pub(crate) cluster: Option<String>,
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
            remote: None,
            staging_time: STAGING_TIME,
            archives: None,
            cpu_memory_gb: None,
            run: String::new(),
            scratch,
            container,
            options: Vec::new(),
            commands: None,
            poll: POLL,
            submit_grace: SUBMIT_GRACE,
            query_grace: crate::queue::QUERY_GRACE,
            queue: Queue::default(),
            made: Images::default(),
            recovery: Mutex::default(),
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

    pub(crate) fn command(&self, name: &str) -> PathBuf {
        match &self.commands {
            Some(folder) => folder.join(name),
            None => PathBuf::from(name),
        }
    }
}

impl Backend for SlurmBackend {
    async fn pin(&self, image: &str) -> Result<Option<String>, ExecutorError> {
        // An image archive has no registry to ask; an image ID is pinned already.
        if archive_path(image).is_some() {
            return Ok(None);
        }
        if image_id(image).is_some() {
            return Ok(Some(image.to_owned()));
        }
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
        crate::script::check_gpus(job.placement)?;
        if job.placement.accelerators.iter().any(|gpu| gpu.count > 0)
            && matches!(&self.container, Container::Engine(_))
        {
            return Err(ExecutorError::Unsupported(
                "SLURM GPU placement requires Apptainer or Singularity".into(),
            ));
        }
        if job.placement.offline
            && matches!(&self.container, Container::Apptainer(program) if !program.ends_with("apptainer"))
        {
            return Err(ExecutorError::Unsupported(
                "the SLURM Singularity executor cannot run a task without network".into(),
            ));
        }
        tokio::select! {
            result = self.admit() => result?,
            () = cancel.cancelled() => return Err(ExecutorError::Cancelled),
        }
        let root = self.scratch.join(job.name);
        let placed = {
            // Without shared folders the inputs go to the node through the staging root.
            let staging = match self.remote {
                Some(_) => Staging::default(),
                None => job.staging.clone(),
            };
            let root = root.clone();
            tokio::task::spawn_blocking(move || place(&root, &staging))
                .await
                .map_err(|error| std::io::Error::other(error.to_string()))?
        };
        let _ownership = placed.as_ref().ok().map(|_| ownership(&root)).transpose()?;
        let mut staged = None;
        let result = match placed {
            // Another attempt has this name; its directory is not ours to remove.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(error.into());
            }
            Err(error) => Err(error.into()),
            Ok(mounts) => self.attempt(job, &root, &mounts, &mut staged, cancel).await,
        };
        if let (Some(remote), Some(staged)) = (&self.remote, &staged)
            && !root.join("remote.json").try_exists()?
        {
            let journal = serde_json::json!({"backend":"SLURM", "locked":true,
                "configuration":self.configuration(), "stopped":true,
                "staging":{"prefix":staged.prefix,"plan":staged.plan,
                    "root":remote.stage.objects.url(""),"cleanup":remote.stage.cleanup}});
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
        let ended = !root.join("remote.json").try_exists().unwrap_or(true);
        if result.is_err() && ended {
            let _ = tokio::fs::remove_dir_all(&root).await;
        }
        if ended {
            let _ = tokio::fs::remove_file(crate::script::engine_marker(&root)).await;
        }
        result
    }
}

/// Where an attempt keeps its objects in the staging root: what it sends back below `prefix`,
/// and its plan at `plan`.
#[derive(Debug)]
struct Staged {
    prefix: String,
    plan: String,
}

impl SlurmBackend {
    /// The command that a container of `image` runs for these arguments.
    async fn task_command(
        &self,
        image: &mut task_model::Image,
        replaces_entrypoint: bool,
        arguments: &[String],
    ) -> Result<Vec<String>, ExecutorError> {
        let file = image.reference.starts_with('/') || image.reference.ends_with(".sif");
        let archive = archive_path(&image.reference).map(Path::to_owned);
        let registry = !file && !image.reference.contains("://") && archive.is_none();
        let selected = if let Some(platform) = image.platform.as_deref() {
            if !registry {
                return Err(ExecutorError::Unsupported(
                    "SLURM cannot select a platform for a SIF file, image archive or image address"
                        .into(),
                ));
            }
            let resolved = self
                .configs
                .resolved_for(&image.reference, Some(platform))
                .await?;
            image.reference = resolved.reference;
            Some(resolved.config)
        } else {
            None
        };
        let reference = image.reference.as_str();
        let configured = registry || archive.is_some();
        let command = match configured && !(replaces_entrypoint && !arguments.is_empty()) {
            true => match (selected, archive) {
                (Some(config), _) => config,
                // The archive is on this machine, also when the nodes get it through staging.
                (None, Some(path)) => tokio::task::spawn_blocking(move || archive_config(&path))
                    .await
                    .map_err(|error| std::io::Error::other(error.to_string()))?
                    .map_err(|reason| ExecutorError::MissingImage {
                        image: reference.to_owned(),
                        reason,
                    })?,
                (None, None) => self.image_config(reference).await?,
            }
            .command(arguments),
            false => arguments.to_vec(),
        };
        if command.is_empty() {
            return Err(ExecutorError::NoCommand(reference.to_owned()));
        }
        Ok(command)
    }

    /// Names the saved archive of an image that a task names by image ID, `sha256:<hex>`.
    fn locate(&self, image: &mut task_model::Image) -> Result<(), ExecutorError> {
        let (Some(folder), Some(hex)) = (&self.archives, image_id(&image.reference)) else {
            return Ok(());
        };
        let file = folder.join(format!("sha256-{hex}.tar"));
        if !file.is_file() {
            return Err(ExecutorError::MissingImage {
                image: image.reference.clone(),
                reason: format!("{} holds no archive of it", folder.display()),
            });
        }
        image.reference = format!("docker-archive:{}", file.display());
        Ok(())
    }

    /// The config of an image of a registry.
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
        let shm = crate::script::shared_memory(&self.container, &spec)?;
        let image = spec.image.as_mut().ok_or(ExecutorError::NoImage)?;
        self.locate(image)?;
        let replaces = spec.replaces_entrypoint;
        spec.arguments = self.task_command(image, replaces, &spec.arguments).await?;
        for container in &mut spec.containers {
            let image = container.image.as_mut().ok_or(ExecutorError::NoImage)?;
            self.locate(image)?;
            let replaces = container.replaces_entrypoint;
            container.arguments = self
                .task_command(image, replaces, &container.arguments)
                .await?;
        }
        let spec = &spec;
        if spec.environment.contains_key(executor::daemon::DAEMON_ENV) {
            return Err(ExecutorError::Unsupported(
                "SLURM jobs cannot run daemons: the run cannot reach them on a network of its own or stop them while it goes on".into(),
            ));
        }
        if spec.containers.iter().any(|container| container.service) {
            return Err(ExecutorError::Unsupported(
                "SLURM jobs cannot run sidecars: their containers share no network of their own and do not stop together".into(),
            ));
        }
        if self.remote.is_some() && !spec.containers.is_empty() {
            return Err(ExecutorError::Unsupported(
                "SLURM object-store staging runs only tasks of one container".into(),
            ));
        }
        let image = spec.image.as_ref().ok_or(ExecutorError::NoImage)?;
        let images =
            std::iter::once(Some(image)).chain(spec.containers.iter().map(|c| c.image.as_ref()));
        let archives = images
            .flatten()
            .any(|image| archive_path(&image.reference).is_some());
        if archives && matches!(&self.container, Container::Engine(_)) {
            return Err(ExecutorError::Unsupported(
                "SLURM runs an image archive only with Apptainer or Singularity; set `registry` in \
                 the `[cwl]` section of the config file for Podman or Docker on the nodes"
                    .into(),
            ));
        }
        // Without shared folders each node pulls the image itself, and an archive comes with
        // the files of the task, into its control folder.
        let archive = archive_path(&image.reference).filter(|_| self.remote.is_some());
        let image = &match (&self.remote, archive) {
            (Some(_), Some(_)) => task_model::Image {
                reference: format!("docker-archive:{}", root.join(ARCHIVE_PATH).display()),
                platform: image.platform.clone(),
            },
            (Some(_), None) => image.clone(),
            (None, _) => self.runnable(image, cancel).await?,
        };
        let control = root.join(CONTROL);
        if matches!(&self.container, Container::Engine(_)) && self.remote.is_none() {
            // The container can write its scratch tree, so its removal proof stays outside it.
            let marker = crate::script::engine_marker(root);
            tokio::fs::write(&marker, "0\n").await?;
            File::open(marker)?.sync_all()?;
            File::open(root.parent().unwrap_or(root))?.sync_all()?;
        }
        let owner = tokio::fs::metadata(root).await?;
        let access = match &self.container {
            // Apptainer runs every container as the user who runs uwe.
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
        crate::script::check_binds(&self.container, &run)?;
        let script = match (&self.remote, spec.containers.is_empty()) {
            (Some(remote), _) => {
                let text = |path: PathBuf| path.to_string_lossy().into_owned();
                let (control, home) = (text(control.clone()), text(root.join(executor::HOME)));
                let (outbox, owner) = (text(root.join("outbox")), access.owner());
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
                    crate::Helper::Upload(binary) => Some(remote.stage.binary(binary).await?),
                    _ => None,
                };
                let files: Vec<(&str, &Path)> = archive
                    .map(|path| (ARCHIVE_PATH, path))
                    .into_iter()
                    .collect();
                let planned = (remote.stage)
                    .plan(
                        job,
                        root,
                        &extra,
                        &files,
                        &records(LOG),
                        access.hand_back.is_some(),
                    )
                    .await?;
                *staged = Some(Staged {
                    prefix: planned.prefix.clone(),
                    plan: planned.plan.clone(),
                });
                let run = ContainerRun {
                    mounts: &planned.mounts,
                    ..run
                };
                crate::script::check_binds(&self.container, &run)?;
                // Without it, a job whose helper cannot start keeps its log on the node.
                let log = match remote.stage.record(&planned.prefix, LOG).await {
                    Ok(log) => Some(log),
                    Err(error) => {
                        tracing::warn!(%error, "jobs cannot send their logs without the helper");
                        None
                    }
                };
                let links = crate::remote::Links {
                    plan: &planned.location,
                    binary: binary.as_deref(),
                    log: log.as_ref(),
                    engine: None,
                };
                let engine = if matches!(&self.container, Container::Engine(_)) {
                    remote
                        .stage
                        .write_record(&planned.prefix, ENGINE_EXIT, b"0\n")
                        .await?;
                    Some(remote.stage.record(&planned.prefix, ENGINE_EXIT).await?)
                } else {
                    None
                };
                let links = crate::remote::Links {
                    engine: engine.as_ref(),
                    ..links
                };
                let time = job.placement.time_seconds;
                crate::remote::job_script(&self.container, &run, remote, gpus, time, links, LOG)
            }
            (None, true) => {
                let wrapper = run.wrapper(spec);
                tokio::fs::write(run.script(), &wrapper).await?;
                let size = wrapper.len() as u64;
                job_script(
                    &self.container,
                    &run,
                    gpus,
                    job.placement.time_seconds,
                    size,
                )
            }
            // A task of several containers starts a container for each image, all at once.
            (None, false) => {
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
                let mut sizes = Vec::new();
                for path in &paths {
                    sizes.push(tokio::fs::metadata(path).await?.len());
                }
                let time = job.placement.time_seconds;
                container_script(&self.container, &runs, &exits, gpus, time, &sizes)
            }
        };
        let script = crate::script::size_memory(&self.container, script, shm);
        tokio::fs::write(control.join(SCRIPT), script).await?;
        // The node reads the wrappers, the copied inputs and the standard input as soon as the
        // job starts.
        let synced = root.to_owned();
        tokio::task::spawn_blocking(move || sync_files(&synced))
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))??;
        let prefix = staged.as_ref().map(|staged| staged.prefix.as_str());
        let script = control.join(SCRIPT);
        // A partition whose time limit leaves no room for the margin of staging gets the job
        // once more without it; the task keeps its own time.
        let mut margin = self.staging_time.as_secs();
        let mut last = None;
        let ended = loop {
            let options = self.job_options(job, root, margin).await;
            let submitted = self.submit(options, &script, root, staged.as_ref(), cancel);
            let ended = match submitted.await? {
                Submission::Taken(job) => {
                    let id = &job.id;
                    self.queue.add(id, job.cluster.as_deref()).await;
                    let ended = self.wait(&job, &control, prefix, cancel).await;
                    last = Some(job.clone());
                    let held = matches!(ended, Ok(Ended::Held(_) | Ended::OverTime));
                    let stopped = self
                        .stop(id, root, self.settle(), ended.is_err() || held)
                        .await;
                    self.queue.remove(id, job.cluster.as_deref()).await;
                    stopped?;
                    ended?
                }
                Submission::OverTime => Ended::OverTime,
            };
            let time = job.placement.time_seconds;
            match (ended, time) {
                (Ended::OverTime, Some(time)) if margin > 0 => {
                    tracing::warn!(
                        job = job.name,
                        time,
                        "the SLURM partition allows no time for staging beyond the time of the task, so the job runs without it and may end before the task does"
                    );
                    margin = 0;
                }
                (Ended::OverTime, _) => {
                    let time =
                        time.map_or("its default time".into(), |time| format!("{time} seconds"));
                    return Err(ExecutorError::Unsupported(format!(
                        "the task asks for {time}, more than its SLURM partition allows"
                    )));
                }
                (ended, _) => break ended,
            }
        };
        let names = records(LOG);
        if let (Some(remote), Some(prefix)) = (&self.remote, prefix)
            && let Err(error) =
                executor::remote::patiently(|| remote.stage.collect(prefix, root, &names)).await
        {
            // The objects stay, as they are the only copy of the results.
            tracing::warn!(prefix, %error, "cannot collect the results of an attempt; they stay in the staging root");
            return Err(error);
        }
        if self.remote.is_some()
            && matches!(ended, Ended::Done(0))
            && read_number(&control.join(CONTAINER_EXIT)).await.is_none()
        {
            return Err(unresolved(root, "the staged completion record is missing"));
        }
        if self.remote.is_some() && root.join("remote.json").try_exists()? {
            let mut journal: serde_json::Value =
                serde_json::from_slice(&std::fs::read(root.join("remote.json"))?)
                    .map_err(|error| unresolved(root, error))?;
            journal["collect"] = serde_json::json!(false);
            journal_at(root, &journal)?;
        }
        // A job that left no log at all ran on a node that does not see the scratch folder.
        let lost = self.remote.is_none()
            && !tokio::fs::try_exists(control.join(LOG))
                .await
                .unwrap_or(true);
        let node = match (lost, &last) {
            (true, Some(id)) => self.leave_out(id).await,
            _ => None,
        };
        let time = job.placement.time_seconds;
        finish(
            ended,
            root.to_owned(),
            time,
            job.spec,
            job.staging,
            self.remote.is_none(),
        )
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

    /// The options of the job of an attempt, whose time is that of its task plus `margin`.
    async fn job_options(&self, job: Job<'_>, root: &Path, margin: u64) -> Vec<String> {
        let mut options = sbatch_options(job, root, margin, &self.options);
        // The options of the user come last, so a comment of theirs wins.
        if !self.run.is_empty() {
            let at = options.len().saturating_sub(self.options.len());
            options.insert(at, format!("--comment={}", comment(&self.run)));
        }
        if let Some(gb) = self.cpu_memory_gb {
            per_cpu_memory(&mut options, gb);
        }
        if self.remote.is_some() {
            // The node has no scratch folder; the job keeps its log in the folder of the node.
            for option in &mut options {
                if option.starts_with("--chdir=") {
                    *option = "--chdir=/tmp".into();
                } else if option.starts_with("--output=") {
                    *option = "--output=/dev/null".into();
                }
            }
        }
        self.with_excluded(options).await
    }

    /// Asks SLURM for the node of a job, and leaves that node out of the later jobs.
    async fn leave_out(&self, job: &Scheduled) -> Option<String> {
        let node = self
            .queue
            .node(
                &self.command("squeue"),
                &self.command("sacct"),
                &job.id,
                job.cluster.as_deref(),
                self.query_grace,
            )
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
    /// pulled once by a job of its own, else the image itself.
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
            (None, None) => archive_file(reference),
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

    /// The SIF file of an image, pulled once by a job of its own.
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
                    Err(ExecutorError::Io(error))
                        if error
                            .to_string()
                            .starts_with("SLURM ownership is unresolved:") =>
                    {
                        return Ok(Err(error.to_string()));
                    }
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
            "image-{}-{}-{}",
            &stem[..stem.len().min(19)],
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |time| time.as_nanos())
        );
        let root = self.scratch.join(&name);
        let control = root.join(CONTROL);
        tokio::fs::create_dir_all(&self.images).await?;
        tokio::fs::create_dir_all(&self.scratch).await?;
        tokio::fs::create_dir(&root).await?;
        let _ownership = ownership(&root)?;
        tokio::fs::create_dir_all(&control).await?;
        // The nodes may not see an archive of this machine, so the job reads a copy of it.
        let source = match archive_path(reference) {
            Some(archive) => {
                let copy = root.join(ARCHIVE);
                if let Err(error) = tokio::fs::copy(archive, &copy).await {
                    let _ = tokio::fs::remove_dir_all(&root).await;
                    return Err(error.into());
                }
                format!("docker-archive:{}", copy.display())
            }
            None => reference.to_owned(),
        };
        let script = pull_script(program, &source, path, &control.join(CONTAINER_EXIT));
        tokio::fs::write(control.join(SCRIPT), script).await?;
        let mut options = pull_options(&name, &root, &self.pull, &self.options);
        if let Some(gb) = self.cpu_memory_gb {
            per_cpu_memory(&mut options, gb);
        }
        let options = self.with_excluded(options).await;
        let script = control.join(SCRIPT);
        let ended = match self.submit(options, &script, &root, None, cancel).await? {
            Submission::Taken(job) => {
                let id = &job.id;
                self.queue.add(id, job.cluster.as_deref()).await;
                let ended = self.wait(&job, &control, None, cancel).await;
                let held = matches!(ended, Ok(Ended::Held(_) | Ended::OverTime));
                let stopped = self
                    .stop(id, &root, self.settle(), ended.is_err() || held)
                    .await;
                self.queue.remove(id, job.cluster.as_deref()).await;
                stopped?;
                ended
            }
            Submission::OverTime => Ok(Ended::OverTime),
        };
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
            Ended::Held(reason) => Err(format!(
                "SLURM held the job that pulls the image {reference} ({reason}), so uwe \
                 cancelled it"
            )),
            Ended::OverTime => Err(format!(
                "the job that pulls the image {reference} asks for more time than its SLURM \
                 partition allows; lower the time of [executor.slurm.pull]"
            )),
        };
        let pulled = match pulled {
            Ok(path) => Ok(path),
            Err(message) => Err(machine(explain(message, &root, LOG, true).await)),
        };
        if root.join("remote.json").try_exists()? {
            let journal = serde_json::from_slice(&std::fs::read(root.join("remote.json"))?)
                .map_err(|error| unresolved(&root, error))?;
            self.settle_journal(&root, &journal).await?;
        }
        let _ = tokio::fs::remove_dir_all(&root).await;
        pulled
    }

    /// Submits a job with its options and script, and gives its id. The journal names the
    /// objects of the attempt in the staging root, which its recovery removes.
    async fn submit(
        &self,
        options: Vec<String>,
        script: &Path,
        root: &Path,
        staged: Option<&Staged>,
        cancel: &CancellationToken,
    ) -> Result<Submission, ExecutorError> {
        if cancel.is_cancelled() {
            return Err(ExecutorError::Cancelled);
        }
        let secret = (self
            .remote
            .as_ref()
            .and_then(|remote| remote.secret_file.as_deref()))
        .map(crate::remote::secret_text)
        .transpose()
        .map_err(std::io::Error::other)?;
        let host = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
        let started = owner_start(std::process::id()).ok_or_else(|| {
            std::io::Error::other("cannot identify the SLURM coordinator process")
        })?;
        let script = std::path::absolute(script)?;
        let mut journal = serde_json::json!({"backend":"SLURM", "commands":self.commands,
            "options":options, "working_directory":std::path::absolute(root)?, "id":null, "cluster":clusters(&options),
            "owner":std::process::id(), "host":host, "started":started, "locked":true,
            "script":script, "identity":null, "configuration":self.configuration(), "run":self.run,
            "engine":matches!(&self.container, Container::Engine(_))});
        if let Some(staged) = staged {
            journal["collect"] = serde_json::json!(true);
            journal["staging"] = serde_json::json!({"prefix":staged.prefix, "plan":staged.plan,
                "root":self.remote.as_ref().map(|remote| remote.stage.objects.url("")),
                "cleanup":self.remote.as_ref().map(|remote| remote.stage.cleanup)});
        }
        journal_at(root, &journal)?;
        let mut sbatch = Command::new(self.command("sbatch"));
        sbatch.env_clear();
        let mut exported = Vec::new();
        for (name, value) in std::env::vars_os() {
            let Some(text) = name.to_str() else { continue };
            if PASSED.contains(&text) || PASSED_PREFIXES.iter().any(|start| text.starts_with(start))
            {
                sbatch.env(text, value);
                exported.push(text.to_owned());
            }
        }
        if let Some(proxy) = &self.proxy {
            for name in ["HTTPS_PROXY", "HTTP_PROXY", "https_proxy", "http_proxy"] {
                sbatch.env(name, proxy);
                exported.push(name.to_owned());
            }
        }
        if let Some(secret) = secret {
            sbatch.env(executor::helper::STORAGE, secret);
            exported.push(executor::helper::STORAGE.to_owned());
        }
        // Names only: `--export` splits at commas, which values may hold. It comes after the
        // options, so that it wins over an `--export` there.
        exported.sort();
        exported.dedup();
        let export = match exported.is_empty() {
            true => "NONE".to_owned(),
            false => exported.join(","),
        };
        sbatch
            .args(options)
            .arg(format!("--export={export}"))
            .arg(&script);
        let output = tokio::select! {
            output = crate::queue::output(&mut sbatch, self.submit_grace) => output.map_err(|error| unresolved(root, error))?,
            () = cancel.cancelled() => return Err(unresolved(root, "cancelled during submission; the response was not recorded")),
        };
        let text = String::from_utf8_lossy(&output.stdout);
        // `--parsable` gives `<id>` or `<id>;<cluster>`.
        let id = text.trim().split(';').next().unwrap_or_default();
        if !output.status.success()
            || id.is_empty()
            || !id.bytes().all(|byte| byte.is_ascii_digit())
        {
            let error = String::from_utf8_lossy(&output.stderr);
            // With EnforcePartLimits sbatch refuses such a job at once, so none is left.
            if !output.status.success() && error.contains("Requested time limit is invalid") {
                journal["stopped"] = serde_json::json!(true);
                journal_at(root, &journal)?;
                return Ok(Submission::OverTime);
            }
            return Err(unresolved(
                root,
                format!("sbatch did not take the job: {}", error.trim()),
            ));
        }
        journal["id"] = serde_json::json!(id);
        if let Some(cluster) = text
            .trim()
            .split(';')
            .nth(1)
            .filter(|cluster| !cluster.is_empty())
        {
            journal["cluster"] = serde_json::json!(cluster);
        }
        journal_at(root, &journal).map_err(|error| unresolved(root, error))?;
        let squeue = self.command("squeue");
        let (_, identity) = tokio::select! {
            value = crate::queue::identified(&squeue, id, journal["cluster"].as_str(), self.query_grace) => value,
            () = cancel.cancelled() => return Err(unresolved(root, "cancelled before scheduler identity was recorded")),
        };
        if let Some(identity) = identity {
            if identity["script"] != journal["script"] {
                return Err(unresolved(
                    root,
                    "the scheduler returned a different script",
                ));
            }
            journal["identity"] = identity;
            journal_at(root, &journal).map_err(|error| unresolved(root, error))?;
        }
        Ok(Submission::Taken(Scheduled {
            id: id.to_owned(),
            cluster: journal["cluster"].as_str().map(str::to_owned),
        }))
    }

    /// How long the end of a job may take to confirm: a few queries, which a slow controller
    /// each answers late, and the time that SLURM takes to stop a job.
    pub(crate) fn settle(&self) -> Duration {
        CANCEL_GRACE.max(self.query_grace * 4)
    }

    pub(crate) fn configuration(&self) -> serde_json::Value {
        let conf = std::env::var("SLURM_CONF").ok();
        let digest = std::fs::read(conf.as_deref().unwrap_or("/etc/slurm/slurm.conf"))
            .ok()
            .map(|bytes| task_model::Digest::sha256(&bytes).to_string());
        serde_json::json!({"commands":self.commands.as_ref().and_then(|path| std::path::absolute(path).ok()),
            "conf":conf, "conf_digest":digest, "server":std::env::var("SLURM_CONF_SERVER").ok(),
            "clusters":std::env::var("SLURM_CLUSTERS").ok(),
            "path":if self.commands.is_none() { std::env::var("PATH").ok() } else { None }})
    }

    /// Recovers matching orphaned tasks and skips proven active local owners.
    pub async fn reconcile(&self) -> Result<(), ExecutorError> {
        let mut scan = self.recovery.lock().await;
        scan.reset();
        while !scan.complete() {
            let entries = scan.page(&self.scratch, 128)?;
            self.reconcile_records(entries).await?;
        }
        Ok(())
    }

    async fn admit(&self) -> Result<(), ExecutorError> {
        let mut scan = self.recovery.lock().await;
        let entries = scan.page(&self.scratch, 128)?;
        let result = self.reconcile_records(entries).await;
        if result.is_err() {
            scan.reset();
        }
        result
    }

    async fn reconcile_records(&self, entries: Vec<PathBuf>) -> Result<(), ExecutorError> {
        for root in entries {
            tokio::task::yield_now().await;
            let path = root.join("remote.json");
            if path.try_exists()? {
                let bytes = match std::fs::read(&path) {
                    Ok(bytes) => bytes,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                };
                let mut journal: serde_json::Value =
                    serde_json::from_slice(&bytes).map_err(|error| unresolved(&root, error))?;
                if journal["backend"] != "SLURM" {
                    return Err(unresolved(
                        &root,
                        "the original backend or coordinator host changed",
                    ));
                }
                let lock = match File::options().write(true).open(root.join("remote.lock")) {
                    Ok(lock) => lock,
                    Err(_) if !path.try_exists()? => continue,
                    Err(error) => return Err(unresolved(&root, error)),
                };
                // The shared lock fences a coordinator across boots and PID namespaces.
                match lock.try_lock() {
                    Err(TryLockError::WouldBlock) if journal["locked"] == true => continue,
                    Err(TryLockError::Error(error)) => return Err(unresolved(&root, error)),
                    Err(TryLockError::WouldBlock) => {
                        return Err(unresolved(
                            &root,
                            "the journal lock has no matching live coordinator",
                        ));
                    }
                    Ok(()) => {}
                }
                let _held = Owned(lock);
                if !path.try_exists()? {
                    continue;
                }
                journal = serde_json::from_slice(&std::fs::read(&path)?)
                    .map_err(|error| unresolved(&root, error))?;
                if journal["backend"] != "SLURM"
                    || journal["locked"] != true
                    || journal["configuration"] != self.configuration()
                {
                    return Err(unresolved(
                        &root,
                        "the original ownership or configuration is insufficient",
                    ));
                }
                if journal["stopped"] == true {
                    self.settle_journal(&root, &journal).await?;
                    continue;
                }
                // A submission without an answer must not block every later run.
                if !journal["identity"].is_object() {
                    match self.identify(&root, &journal).await {
                        Submitted::Found(job, identity) => {
                            journal["id"] = serde_json::json!(job.id);
                            journal["cluster"] = serde_json::json!(job.cluster);
                            journal["identity"] = identity;
                            journal_at(&root, &journal)?;
                        }
                        Submitted::Absent => {
                            self.engine_stopped(&root, &journal).await?;
                            tracing::warn!(attempt = %root.display(),
                                "the job of an unclear submission is not in the queue; the attempt is lost");
                            journal["stopped"] = serde_json::json!(true);
                            journal_at(&root, &journal)?;
                            self.settle_journal(&root, &journal).await?;
                            continue;
                        }
                        Submitted::Unclear(reason) => {
                            tracing::warn!(attempt = %root.display(), reason,
                                "cannot resolve an unclear submission yet");
                            continue;
                        }
                    }
                }
                if !journal["identity"].is_object()
                    || journal["identity"]["script"] != journal["script"]
                {
                    return Err(unresolved(
                        &root,
                        "the original ownership, configuration or scheduler identity is insufficient",
                    ));
                }
                let id = journal["id"]
                    .as_str()
                    .filter(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
                    .ok_or_else(|| unresolved(&root, "the accepted job ID was not recorded"))?
                    .to_owned();
                let (look, identity) = crate::queue::identified(
                    &self.command("squeue"),
                    &id,
                    journal["cluster"].as_str(),
                    self.query_grace,
                )
                .await;
                if identity
                    .as_ref()
                    .is_some_and(|identity| identity != &journal["identity"])
                    || (identity.is_none() && look != Look::Gone)
                {
                    return Err(unresolved(
                        &root,
                        "the original scheduler identity cannot be confirmed",
                    ));
                }
                journal["owner"] = serde_json::json!(std::process::id());
                journal["started"] =
                    serde_json::json!(owner_start(std::process::id()).ok_or_else(
                        || unresolved(&root, "cannot identify the recovery coordinator")
                    )?);
                journal_at(&root, &journal)?;
                self.stop(&id, &root, self.settle(), true).await?;
                let journal = serde_json::from_slice(&std::fs::read(&path)?)
                    .map_err(|error| unresolved(&root, error))?;
                self.settle_journal(&root, &journal).await?;
            }
        }
        Ok(())
    }

    /// Removes the objects in the staging root that the journal of an attempt names.
    pub(crate) async fn settle_journal(
        &self,
        root: &Path,
        journal: &serde_json::Value,
    ) -> Result<(), ExecutorError> {
        let staging = &journal["staging"];
        let (prefix, plan) = (staging["prefix"].as_str(), staging["plan"].as_str());
        if let (Some(prefix), Some(plan)) = (prefix, plan) {
            let remote = self
                .remote
                .as_ref()
                .ok_or_else(|| unresolved(root, "the staging configuration is missing"))?;
            if staging["root"].as_str() != Some(remote.stage.objects.url("").as_str()) {
                return Err(unresolved(root, "the staging root changed"));
            }
            let mut stage = remote.stage.clone();
            stage.cleanup = staging["cleanup"].as_bool().unwrap_or(stage.cleanup);
            if journal["collect"] == true {
                stage.collect(prefix, root, &records(LOG)).await?;
                let mut journal = journal.clone();
                journal["collect"] = serde_json::json!(false);
                journal_at(root, &journal)?;
            }
            stage.try_clean(prefix, plan).await?;
        }
        std::fs::remove_file(root.join("remote.json"))?;
        File::open(root)?.sync_all()?;
        let _ = std::fs::remove_file(crate::script::engine_marker(root));
        Ok(())
    }

    /// Finds the job of a journal whose submission left no scheduler identity: by its recorded
    /// id, or else by its name and script, which only this attempt has, among the jobs of the
    /// owner of its folder. The job must have the name and script of the journal.
    pub(crate) async fn identify(&self, root: &Path, journal: &serde_json::Value) -> Submitted {
        let age = self.poll * 4 + self.query_grace + self.submit_grace;
        if !executor::poll::old_enough(&root.join("remote.json"), age) {
            return Submitted::Unclear("the submission is too recent to tell");
        }
        // A job counts as absent only when two answers in a row lack it.
        match self.look_up(root, journal).await {
            Submitted::Absent => {
                tokio::time::sleep(self.poll).await;
                self.look_up(root, journal).await
            }
            found => found,
        }
    }

    /// One answer of the queue about the job of a journal.
    async fn look_up(&self, root: &Path, journal: &serde_json::Value) -> Submitted {
        let choices: Vec<_> = match journal["cluster"].as_str() {
            Some(cluster) => cluster.split(',').map(Some).collect(),
            None => vec![None],
        };
        let mut found = None;
        for cluster in choices {
            match self.look_cluster(root, journal, cluster).await {
                Submitted::Absent => {}
                Submitted::Found(job, identity) if found.is_none() => found = Some((job, identity)),
                Submitted::Found(_, _) => {
                    return Submitted::Unclear("several clusters have the job");
                }
                unclear => return unclear,
            }
        }
        match found {
            Some((job, identity)) => Submitted::Found(job, identity),
            None => Submitted::Absent,
        }
    }

    async fn look_cluster(
        &self,
        root: &Path,
        journal: &serde_json::Value,
        cluster: Option<&str>,
    ) -> Submitted {
        let squeue = self.command("squeue");
        let options = journal["options"].as_array().into_iter().flatten();
        let name = (options.filter_map(|option| option.as_str()))
            .find_map(|option| option.strip_prefix("--job-name="));
        let (Some(name), Some(script)) = (name, journal["script"].as_str()) else {
            return Submitted::Unclear("the journal names no job name or script");
        };
        let id = match journal["id"].as_str() {
            Some(id) => id.to_owned(),
            None => {
                let Ok(folder) = std::fs::metadata(root) else {
                    return Submitted::Unclear("the folder of the attempt is gone");
                };
                let grace = self.query_grace;
                match crate::queue::named(&squeue, name, script, folder.uid(), cluster, grace).await
                {
                    None => return Submitted::Unclear("squeue cannot list the jobs of the name"),
                    Some(ids) if ids.is_empty() => return Submitted::Absent,
                    Some(ids) if ids.len() == 1 => ids[0].clone(),
                    Some(_) => return Submitted::Unclear("several jobs have the name and script"),
                }
            }
        };
        match crate::queue::identified(&squeue, &id, cluster, self.query_grace).await {
            (_, Some(identity)) if identity["name"] == name && identity["script"] == script => {
                Submitted::Found(
                    Scheduled {
                        id,
                        cluster: cluster.map(str::to_owned),
                    },
                    identity,
                )
            }
            (_, Some(_)) => Submitted::Unclear("the job has another name or script"),
            (Look::Gone, None) => Submitted::Absent,
            (_, None) => Submitted::Unclear("squeue cannot identify the job"),
        }
    }

    /// Waits for a job outcome or cancellation before confirming termination. A job that sends
    /// its records to `prefix` of the staging root shows its end there.
    async fn wait(
        &self,
        job: &Scheduled,
        control: &Path,
        prefix: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<Ended, ExecutorError> {
        let id = job.id.as_str();
        let cluster = job.cluster.as_deref();
        let squeue = self.command("squeue");
        let sacct = self.command("sacct");
        let mut poll = Poll::new(self.poll);
        let mut ended: Option<Option<String>> = None;
        loop {
            tracing::debug!(job = id, "polling SLURM job");
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
                look = self.queue.look(id, cluster, self.poll, &squeue, self.query_grace) => look,
                () = cancel.cancelled() => {
                    return Err(ExecutorError::Cancelled);
                }
            };
            let state = match look {
                Look::Active | Look::Unknown => continue,
                Look::Held(reason) => return Ok(Ended::Held(reason)),
                Look::OverTime => return Ok(Ended::OverTime),
                Look::Ended(state) => Some(state),
                Look::Gone => tokio::select! {
                    state = self.queue.accounted(&sacct, id, cluster, self.query_grace) => state,
                    () = cancel.cancelled() => {
                        return Err(ExecutorError::Cancelled);
                    }
                },
            };
            // A job that ended well has recorded its container; the shared file system may
            // show the record late. Other ends need no wait.
            match state.as_deref() {
                Some("COMPLETED") | None => {
                    ended = Some(state);
                    poll.troubled(true, EXIT_GRACE);
                }
                Some(_) => return Ok(Ended::Stopped(state)),
            }
        }
    }

    /// Records termination after an independent query, retaining pending data obligations.
    pub(crate) async fn stop(
        &self,
        id: &str,
        root: &Path,
        grace: Duration,
        cancel: bool,
    ) -> Result<(), ExecutorError> {
        let mut journal: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.join("remote.json")).map_err(|error| unresolved(root, error))?,
        )
        .map_err(|error| unresolved(root, error))?;
        if journal["stopped"] == true {
            return Ok(());
        }
        if journal["configuration"] != self.configuration() {
            return Err(unresolved(root, "the scheduler configuration changed"));
        }
        if !journal["identity"].is_object() {
            let identity = executor::poll::until_stopped(
                grace,
                self.poll.min(Duration::from_secs(1)),
                || async {
                    let cluster = journal["cluster"].as_str();
                    let (look, identity) = crate::queue::identified(
                        &self.command("squeue"),
                        id,
                        cluster,
                        self.query_grace,
                    )
                    .await;
                    if let Some(identity) = identity {
                        return Some(if identity["script"] == journal["script"] {
                            Ok(Some(identity))
                        } else {
                            Err(unresolved(
                                root,
                                "the scheduler returned a different script",
                            ))
                        });
                    }
                    if look == Look::Gone
                        && crate::queue::look_one(
                            &self.command("squeue"),
                            id,
                            cluster,
                            self.query_grace,
                        )
                        .await
                            == Look::Gone
                    {
                        return Some(Ok(None));
                    }
                    None
                },
            )
            .await
            .map_err(|_| unresolved(root, "scheduler identity acquisition timed out"))??;
            if let Some(identity) = identity {
                journal["identity"] = identity;
                journal_at(root, &journal)?;
            }
        }
        let cluster = journal["cluster"].as_str();
        executor::poll::until_stopped(grace, self.poll.min(Duration::from_secs(1)), || async {
            let (look, identity) =
                crate::queue::identified(&self.command("squeue"), id, cluster, self.query_grace)
                    .await;
            let identified = identity.is_some();
            if let Some(identity) = identity {
                if identity != journal["identity"] || identity["script"] != journal["script"] {
                    return Some(Err(unresolved(
                        root,
                        "the scheduler job identity changed or was not recorded",
                    )));
                }
            } else if look != Look::Gone {
                return None;
            }
            if matches!(look, Look::Ended(_) | Look::Gone)
                && matches!(
                    crate::queue::look_one(&self.command("squeue"), id, cluster, self.query_grace)
                        .await,
                    Look::Ended(_) | Look::Gone
                )
            {
                return Some(Ok(()));
            }
            if cancel && identified {
                let mut command = Command::new(self.command("scancel"));
                // The name filter is not an atomic submit-time fence; fresh identity remains required.
                command.arg(id).arg("--ctld").arg(format!(
                    "--name={}",
                    journal["identity"]["name"].as_str().unwrap_or_default()
                ));
                if let Some(cluster) = cluster {
                    command.arg(format!("--clusters={cluster}"));
                }
                let _ = crate::queue::output(&mut command, self.query_grace).await;
            }
            None
        })
        .await
        .map_err(|_| unresolved(root, format!("job {id} termination was not confirmed")))??;
        self.engine_stopped(root, &journal).await?;
        journal["stopped"] = serde_json::json!(true);
        journal_at(root, &journal)?;
        Ok(())
    }

    pub(crate) async fn engine_stopped(
        &self,
        root: &Path,
        journal: &serde_json::Value,
    ) -> Result<(), ExecutorError> {
        if let Some(recorded) = journal["staging"]["root"].as_str()
            && self
                .remote
                .as_ref()
                .map(|remote| remote.stage.objects.url(""))
                .as_deref()
                != Some(recorded)
        {
            return Err(unresolved(
                root,
                "the original staging root is not configured",
            ));
        }
        if journal["engine"] == true {
            let mut marker = crate::script::engine_marker(root);
            if let (Some(remote), Some(prefix)) =
                (&self.remote, journal["staging"]["prefix"].as_str())
            {
                if !remote.stage.fetch(prefix, ENGINE_EXIT, root).await? {
                    return Err(unresolved(
                        root,
                        "the node has not confirmed container removal",
                    ));
                }
                marker = root.join(CONTROL).join(ENGINE_EXIT);
            }
            if read_number(&marker).await != Some(0) {
                return Err(unresolved(
                    root,
                    "the node has not confirmed container removal",
                ));
            }
        }
        Ok(())
    }
}

/// What became of a submission whose answer was lost.
pub(crate) enum Submitted {
    /// The job with this id and identity.
    Found(Scheduled, serde_json::Value),
    /// SLURM has no such job: it never took it, or it ended long ago.
    Absent,
    /// The queue cannot tell yet, for this reason.
    Unclear(&'static str),
}

/// The comment of the jobs of the run `run`, which tells the run of a job to whoever lists it.
pub(crate) fn comment(run: &str) -> String {
    format!("uwe run {run}")
}

fn clusters(options: &[String]) -> Option<String> {
    let mut cluster = std::env::var("SLURM_CLUSTERS").ok();
    let mut words = options.iter();
    while let Some(word) = words.next() {
        if matches!(word.as_str(), "--clusters" | "-M") {
            cluster = words.next().cloned();
        } else if let Some(value) = word
            .strip_prefix("--clusters=")
            .or_else(|| word.strip_prefix("-M"))
        {
            cluster = Some(value.to_owned());
        }
    }
    cluster
}

/// The lock of an attempt. It lets go of the lock before it closes the file, as a process that
/// this one starts holds a copy of the file, and so the lock, until it runs its program.
pub(crate) struct Owned(pub(crate) File);

impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn ownership(root: &Path) -> Result<Owned, ExecutorError> {
    let lock = File::options()
        .write(true)
        .create_new(true)
        .open(root.join("remote.lock"))?;
    lock.try_lock()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(Owned(lock))
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
        "SLURM ownership is unresolved: {error}; retained {}",
        root.join("remote.json").display()
    ))
    .into()
}

pub(crate) fn journal_at(root: &Path, journal: &serde_json::Value) -> Result<(), ExecutorError> {
    use std::io::Write;
    let path = root.join("remote.json.next");
    let mut file = std::fs::File::create(&path)?;
    file.write_all(journal.to_string().as_bytes())?;
    file.sync_all()?;
    std::fs::rename(path, root.join("remote.json"))?;
    std::fs::File::open(root)?.sync_all()?;
    Ok(())
}

/// What an attempt left behind, by how its job ended.
async fn finish(
    ended: Ended,
    root: PathBuf,
    time: Option<u64>,
    spec: &task_model::TaskSpec,
    staging: &Staging,
    shared: bool,
) -> Result<Finished, ExecutorError> {
    let control = root.join(CONTROL);
    let recorded = match ended {
        Ended::Done(0) => record(spec, &control).await,
        _ => containers::recorded(spec, &control),
    };
    let (exit_code, note) = match outcome(recorded, &ended, time) {
        Ok(outcome) => outcome,
        Err(message) => return Err(machine(explain(message, &root, LOG, shared).await)),
    };
    if let Some(note) = note {
        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(control.join(STDERR))
            .await?;
        file.write_all(note.as_bytes()).await?;
        // A file of Tokio writes in the background until it is flushed.
        file.flush().await?;
    }
    let files = match exit_code {
        0 => {
            let manifest = tokio::fs::read_to_string(control.join(MANIFEST)).await;
            parse(&manifest.unwrap_or_default())?
        }
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
        finished.collect_outputs(spec, staging)?;
    }
    Ok(finished)
}

/// The exit code that the wrapper of a task recorded in a container that ended well. The node
/// wrote it before the record of the container, but a shared file system may show it to this
/// machine later, so it is looked for again for a while before the attempt counts as failed.
pub(crate) async fn record(spec: &task_model::TaskSpec, control: &Path) -> Option<i32> {
    let since = Instant::now();
    let mut pause = Duration::from_millis(100);
    loop {
        if let Some(code) = containers::recorded(spec, control) {
            return Some(code);
        }
        if since.elapsed() >= EXIT_GRACE {
            return None;
        }
        tokio::time::sleep(pause).await;
        pause = (pause * 2).min(Duration::from_secs(2));
    }
}

/// Syncs every file in a folder, and in its folders, but not the files that links point to: on
/// a shared file system like CephFS, a file that one machine wrote and did not sync may show
/// empty or short on another machine for a while.
pub(crate) fn sync_files(folder: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(folder)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            sync_files(&entry.path())?;
        } else if kind.is_file() {
            std::fs::File::open(entry.path())?.sync_all()?;
        }
    }
    Ok(())
}

/// The exit code of the task and a note for its standard error, from the exit code that the
/// wrapper recorded and the end of the job; `Err` when the machine failed rather than the task.
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
        Ended::Stopped(Some(state)) if state == "TIMEOUT" => Ok((143, Some(too_long(time)))),
        Ended::Stopped(Some(state)) if state == "OUT_OF_MEMORY" => Ok((
            137,
            Some("uwe: SLURM stopped the task for lack of memory\n".to_owned()),
        )),
        Ended::Stopped(Some(state)) => Err(format!("the job ended as {state}")),
        Ended::Held(reason) => Err(format!(
            "SLURM held the job ({reason}), so uwe cancelled it"
        )),
        Ended::OverTime => Err("the job asks for more time than its partition allows".into()),
        Ended::Stopped(None) => Err("the job ended without a record of its container".into()),
    }
}

/// The note of a task that ran longer than its time, or than SLURM allows.
fn too_long(time: Option<u64>) -> String {
    match time {
        Some(time) => format!("uwe: the task ran longer than its time of {time} seconds\n"),
        None => "uwe: the task ran longer than SLURM allows\n".to_owned(),
    }
}

/// A failure of the machine with the end of the job's log, or a hint when the job left none.
/// A job on a node without the `shared` folders keeps its log beside its folder on the node
/// when it could not send it back.
pub(crate) async fn explain(message: String, root: &Path, log_file: &str, shared: bool) -> String {
    let log = tokio::fs::read_to_string(root.join(CONTROL).join(log_file)).await;
    if log.is_err() && !shared {
        return format!(
            "{message}, and sent back no log; look for uwe-{}.*.log in the node folder \
             of the node",
            root.file_name().unwrap_or_default().to_string_lossy()
        );
    }
    let Ok(log) = log else {
        return format!(
            "{message}, and left no log; do the nodes see {} at the same path?",
            root.display()
        );
    };
    let lines: Vec<&str> = log.lines().filter(|line| !line.trim().is_empty()).collect();
    match lines.len() {
        0 => format!("{message}, and its log is empty"),
        count => format!("{message}: {}", lines[count.saturating_sub(5)..].join("; ")),
    }
}

pub(crate) async fn read_number(path: &Path) -> Option<i32> {
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

    #[tokio::test]
    async fn retains_cleanup_failure() {
        let root = tempfile::tempdir().unwrap();
        let staging = root.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("cas"), "blocks deletion").unwrap();
        let objects = content_store::Store::local(&staging).unwrap();
        let attempt = root.path().join("attempt");
        std::fs::create_dir(&attempt).unwrap();
        let mut backend =
            SlurmBackend::new(root.path().into(), Container::Apptainer("apptainer".into()));
        backend.remote = Some(NodeStaging {
            stage: executor::remote::RemoteStage {
                objects,
                expiry: Duration::from_secs(60),
                max_outputs: 1,
                credentials: false,
                cleanup: true,
            },
            helper: crate::Helper::Path("/bin/false".into()),
            folder: None,
            secret_file: None,
        });
        let journal = serde_json::json!({"stopped":true,"staging":{"prefix":"attempts/test",
            "plan":"cas/plan","root":backend.remote.as_ref().unwrap().stage.objects.url(""),"cleanup":true}});
        journal_at(&attempt, &journal).unwrap();
        assert!(backend.settle_journal(&attempt, &journal).await.is_err());
        assert!(attempt.join("remote.json").exists());
        std::fs::remove_file(staging.join("cas")).unwrap();
        std::fs::create_dir(staging.join("cas")).unwrap();
        std::fs::write(staging.join("cas/plan"), "plan").unwrap();
        backend.settle_journal(&attempt, &journal).await.unwrap();
        assert!(!attempt.join("remote.json").exists());
        assert!(!staging.join("cas/plan").exists());
    }

    #[tokio::test]
    async fn recovers_selected_cluster() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let mut backend = SlurmBackend::new(
            root.path().to_owned(),
            Container::Apptainer("apptainer".into()),
        );
        backend.commands = Some(root.path().to_owned());
        let tool = root.path().join("squeue");
        std::fs::write(&tool, r#"#!/bin/bash
case "$*" in *'--clusters=alpha'*) [ -e "$0.duplicate" ] || exit 0 ;; *'--clusters=beta'*) ;; *) exit 1 ;; esac
case "$*" in
    *'--format=%i|%o'*) echo '7|/script' ;;
    *) echo '7|RUNNING|uwe-owner|2026-10-03T00:00:00|/script' ;;
esac
"#).unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let journal = serde_json::json!({"id":null, "cluster":"alpha,beta", "script":"/script", "options":["--job-name=uwe-owner"]});
        match backend.look_up(root.path(), &journal).await {
            Submitted::Found(job, _) => {
                assert_eq!(job.id, "7");
                assert_eq!(job.cluster.as_deref(), Some("beta"));
            }
            _ => panic!("the selected cluster must retain the lost submission"),
        }
        std::fs::write(root.path().join("squeue.duplicate"), "").unwrap();
        assert!(matches!(
            backend.look_up(root.path(), &journal).await,
            Submitted::Unclear(_)
        ));
        assert_eq!(
            clusters(&["--clusters".into(), "alpha,beta".into()]).as_deref(),
            Some("alpha,beta")
        );
        assert_eq!(clusters(&["-Mbeta".into()]).as_deref(), Some("beta"));
    }

    #[tokio::test]
    async fn preserves_replacement_identity() {
        use std::os::unix::fs::PermissionsExt;
        let folder = tempfile::tempdir().unwrap();
        let mut backend = SlurmBackend::new(
            folder.path().to_owned(),
            Container::Apptainer("apptainer".into()),
        );
        backend.commands = Some(folder.path().to_owned());
        backend.poll = Duration::from_millis(10);
        for (name, script) in [
            (
                "squeue",
                r#"case "$*" in
    *'--format=%i|%T|%j|%V|%o'*)
        if [ -e "$0.once" ]; then
            echo '101|RUNNING|replacement|2026-10-03T00:00:01|/other'
        else
            touch "$0.once"
        fi ;;
    *) echo '101 RUNNING' ;;
esac"#,
            ),
            ("scancel", "touch \"$0.called\""),
        ] {
            let path = folder.path().join(name);
            std::fs::write(&path, format!("#!/bin/bash\n{script}\n")).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        journal_at(folder.path(), &serde_json::json!({
            "id":"101", "cluster":null, "script":"/script", "configuration":backend.configuration(),
            "identity":{"name":"owner","submitted":"2026-10-03T00:00:00","script":"/script"}
        })).unwrap();
        assert!(
            backend
                .stop("101", folder.path(), Duration::from_secs(1), true)
                .await
                .is_err()
        );
        assert!(folder.path().join("remote.json").is_file());
        assert!(!folder.path().join("scancel.called").exists());
    }

    #[tokio::test]
    async fn names_node_folder() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().join("run-P-1-1");
        let text = explain("the job failed".into(), &root, LOG, false).await;
        let hint = "look for uwe-run-P-1-1.*.log in the node folder";
        assert!(text.contains(hint), "{text}");
    }

    #[tokio::test]
    async fn preserves_foreign_lock() {
        let folder = tempfile::tempdir().unwrap();
        let backend = SlurmBackend::new(
            folder.path().to_owned(),
            Container::Apptainer("apptainer".into()),
        );
        let root = folder.path().join("orphan");
        std::fs::create_dir(&root).unwrap();
        let journal = serde_json::json!({"backend":"SLURM", "host":"foreign-boot", "locked":true,
            "owner":0, "started":"0", "id":null});
        journal_at(&root, &journal).unwrap();
        let held = File::create(root.join("remote.lock")).unwrap();
        held.try_lock().unwrap();
        backend.reconcile().await.unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("remote.json")).unwrap(),
            journal.to_string()
        );
    }

    #[tokio::test]
    async fn cancellation_preserves_ownership() {
        use std::os::unix::fs::PermissionsExt;
        let folder = tempfile::tempdir().unwrap();
        let mut backend = SlurmBackend::new(
            folder.path().to_owned(),
            Container::Apptainer("apptainer".into()),
        );
        backend.commands = Some(folder.path().to_owned());
        for script in ["exit 1", "exec sleep 30"] {
            for (name, script) in [
                ("scancel", script),
                (
                    "squeue",
                    "echo '101|RUNNING|owner|2026-10-03T00:00:00|/script'",
                ),
            ] {
                let path = folder.path().join(name);
                std::fs::write(&path, format!("#!/bin/bash\n{script}\n")).unwrap();
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            journal_at(
                folder.path(),
                &serde_json::json!({"id":"101", "cluster":null, "script":"/script", "configuration":backend.configuration(),
                    "identity":{"name":"owner","submitted":"2026-10-03T00:00:00","script":"/script"}}),
            )
            .unwrap();
            let error = backend
                .stop("101", folder.path(), Duration::from_millis(100), true)
                .await
                .unwrap_err();
            assert!(!error.retryable());
            assert!(folder.path().join("remote.json").is_file());
        }
        let foreign = folder.path().join("foreign");
        std::fs::create_dir(&foreign).unwrap();
        std::fs::write(foreign.join("remote.json"), "{\"backend\":\"other\"}").unwrap();
        assert!(backend.reconcile().await.is_err());
        assert!(foreign.join("remote.json").is_file());
    }

    #[tokio::test]
    async fn pins_selected_platform() {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap().to_owned();
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                }
                let body = match path.as_str() {
                    "/v2/tool/manifests/1" => {
                        r#"{"manifests":[{"digest":"sha256:aa","platform":{"os":"linux","architecture":"arm64"}},{"digest":"sha256:bb","platform":{"os":"linux","architecture":"amd64"}}]}"#
                    }
                    "/v2/tool/manifests/sha256:aa" => r#"{"config":{"digest":"sha256:cc"}}"#,
                    "/v2/tool/blobs/sha256:cc" => {
                        r#"{"os":"linux","architecture":"arm64","config":{"Entrypoint":["arm-entry"],"Cmd":["arm-command"]}}"#
                    }
                    _ => panic!("unexpected request {path}"),
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let backend = SlurmBackend::new(PathBuf::new(), Container::Apptainer("apptainer".into()));
        let mut image = task_model::Image {
            reference: format!("localhost:{port}/tool:1"),
            platform: Some("linux/arm64".into()),
        };
        let command = backend
            .task_command(&mut image, true, &["custom".into()])
            .await
            .unwrap();
        assert_eq!(command, ["custom"]);
        assert_eq!(image.reference, format!("localhost:{port}/tool@sha256:aa"));
        let mut image = task_model::Image {
            reference: format!("localhost:{port}/tool:1"),
            platform: Some("linux/arm64".into()),
        };
        assert_eq!(
            backend.task_command(&mut image, false, &[]).await.unwrap(),
            ["arm-entry", "arm-command"]
        );
        server.join().unwrap();
    }

    #[tokio::test]
    async fn rejects_engine_gpus() {
        let folder = tempfile::tempdir().unwrap();
        let scratch = folder.path().join("scratch");
        let backend = SlurmBackend::new(
            scratch.clone(),
            Container::Engine(executor::EngineCommand::docker("docker")),
        );
        let spec = task_model::TaskSpec {
            format: task_model::FORMAT,
            namespace: String::new(),
            image: None,
            arguments: Vec::new(),
            workdir: None,
            environment: Default::default(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            output_manifest: None,
            stdin: None,
            containers: Vec::new(),
            replaces_entrypoint: false,
            run_as: None,
        };
        let staging = executor::Staging::default();
        let mut placement = task_model::Placement::default();
        placement.accelerators.push(task_model::Accelerator {
            count: 1,
            kind: None,
        });
        let job = Job {
            name: "gpu",
            spec: &spec,
            staging: &staging,
            placement: &placement,
        };
        assert!(matches!(
            backend.run(job, &CancellationToken::new()).await,
            Err(ExecutorError::Unsupported(_))
        ));
        assert!(!scratch.exists());
    }

    #[test]
    fn classifies_job_failures() {
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
        assert_eq!(outcome(Some(2), &done(2), None), Ok((2, None)));
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
