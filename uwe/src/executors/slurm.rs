//! Builds SLURM container executors with shared store and scratch paths, or with object-store
//! staging.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use content_store::Store;
use executor::{EngineCommand, EngineKind};
use slurm_executor::{Container, PullResources, SlurmBackend};
use storage::Storage;
use task_runner::{Runner, RunnerExecutor};

use crate::Failure;
use crate::arguments::Arguments;
use crate::setup::Setup;

/// The container runtime of the nodes that `-slurm-container` names: `apptainer` (the
/// default), `singularity`, `podman` or `docker`.
pub fn container(name: Option<&str>) -> Result<Container, String> {
    match name.unwrap_or("apptainer") {
        name @ ("apptainer" | "singularity") => Ok(Container::Apptainer(name.to_owned())),
        "podman" => Ok(Container::Engine(EngineCommand {
            program: "podman".into(),
            kind: EngineKind::Podman,
            rootless: true,
        })),
        "docker" => Ok(Container::Engine(EngineCommand::docker("docker"))),
        other => Err(format!(
            "unknown container runtime `{other}` for SLURM; use apptainer, singularity, podman \
             or docker"
        )),
    }
}

/// The backend whose jobs run their tasks with `container` in `<scratch>/slurm`, with the
/// `sbatch` options `options`; a job that pulls an image asks for `pull`. The jobs get the web
/// proxy of the config file.
pub fn backend(
    scratch: &Path,
    container: Container,
    options: Vec<String>,
    pull: PullResources,
) -> SlurmBackend {
    let mut backend = SlurmBackend::new(scratch.join("slurm"), container);
    backend.options = options;
    backend.pull = pull;
    backend.proxy = storage::config_proxy().map(str::to_owned);
    backend.archives = Some(crate::cwl_archives(&crate::cache_directory()));
    backend
}

/// The backend of the container, options and staging of the command line and the config file,
/// with its attempts in `<scratch>/slurm`, for the run `run`, which owns its jobs whether the
/// backend runs every task or those of one site.
pub fn configured(
    arguments: &Arguments,
    setup: &Setup,
    scratch: &Path,
    run: &str,
) -> Result<SlurmBackend, Failure> {
    let settings = &setup.executors;
    let named = (arguments.slurm_container.as_deref()).or(settings.slurm_container.as_deref());
    let container =
        container(named).map_err(|message| Failure::usage(format!("uwe: {message}")))?;
    let options = match arguments.slurm_options.as_deref() {
        Some(options) => options.split_whitespace().map(str::to_owned).collect(),
        None => settings.slurm_options.clone().unwrap_or_default(),
    };
    let mut backend = backend(scratch, container, options, settings.slurm_pull);
    backend.run = run.to_owned();
    // The jobs reach the web as the storage of this run does.
    backend.proxy = setup.storage.proxy().map(str::to_owned);
    backend.archives = Some(crate::cwl_archives(&setup.cache_directory()));
    backend.remote = remote(settings, &setup.storage)
        .map_err(|message| Failure::setup(format!("uwe: {message}")))?;
    backend.staging_time = settings.slurm_staging.margin();
    backend.cpu_memory_gb = settings.slurm_cpu_memory;
    backend.host_pull = settings.slurm_host_pull;
    if let Some(folder) = &settings.slurm_images {
        backend.images = folder.clone();
    }
    if let Some(seconds) = settings.slurm_query {
        backend.query_grace = std::time::Duration::from_secs(seconds);
    }
    Ok(backend)
}

/// How the jobs get their files when the config file names object-store staging; `None` when
/// the nodes see the scratch folder at the same path.
pub fn remote(
    executors: &crate::setup::Executors,
    storage: &Storage,
) -> Result<Option<slurm_executor::NodeStaging>, String> {
    let Some(stage) = executors.slurm_staging.stage(storage)? else {
        return Ok(None);
    };
    let helper = match (&executors.slurm_helper, &executors.slurm_helper_file) {
        (Some(helper), _) => helper.clone(),
        (None, Some(file)) => slurm_executor::Helper::Upload(
            executor::binary::Binary::open(file)
                .map_err(|error| format!("{error}, or set helper_path or helper_url"))?,
        ),
        (None, None) => return Err("SLURM object-store staging needs helper_path".into()),
    };
    if let Some(file) = &executors.slurm_secret_file {
        slurm_executor::secret_text(file)?;
    }
    Ok(Some(slurm_executor::NodeStaging {
        stage,
        helper,
        folder: executors.slurm_node_folder.clone(),
        secret_file: executors.slurm_secret_file.clone(),
    }))
}

/// An executor that submits each task as a batch job of `backend`. Tasks without an image get
/// `default_image`.
pub fn executor(
    store: Store,
    mut backend: SlurmBackend,
    run: &str,
    default_image: Option<String>,
    storage: Storage,
    setup: &crate::setup::Setup,
) -> Result<RunnerExecutor<crate::executors::conda::Remote<SlurmBackend>>, String> {
    let needed: Vec<&str> = (backend.missing_tools().into_iter())
        .filter(|tool| *tool != "sacct")
        .collect();
    if let Some((last, others)) = needed.split_last() {
        let (tools, are) = match others {
            [] => (format!("`{last}` is"), ""),
            others => (format!("`{}` and `{last}`", others.join("`, `")), " are"),
        };
        return Err(format!(
            "-executor slurm needs the tools of SLURM, but {tools}{are} not on the PATH; run \
             uwe on a node of the SLURM cluster, like its login node"
        ));
    }
    backend.run = run.to_owned();
    let runtime = workflow_graph::runtime().map_err(|error| error.to_string())?;
    runtime
        .block_on(backend.reconcile())
        .map_err(|error| error.to_string())?;
    // The nodes read the inputs from the store, so they are copied there.
    let scratch = backend.scratch.clone();
    let backend =
        crate::executors::conda::Remote::new(backend, store.clone(), &scratch, run, setup);
    let context = crate::run::RunContext {
        storage,
        ..setup.context()
    };
    let mut runner = Runner::new(store, backend, run, crate::runner_settings(&context, true));
    runner.references(setup.references.clone());
    Ok(RunnerExecutor::new(runner, default_image))
}
