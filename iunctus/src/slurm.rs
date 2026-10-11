//! Builds SLURM container executors with shared store and scratch paths, or with object-store
//! staging.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use content_store::Store;
use executor::{EngineCommand, EngineKind};
use slurm_executor::{Container, PullResources, SlurmBackend};
use storage::Storage;
use task_runner::{Runner, RunnerExecutor};

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
    backend
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
    backend: SlurmBackend,
    run: &str,
    default_image: Option<String>,
    storage: Storage,
    setup: &crate::setup::Setup,
) -> Result<RunnerExecutor<crate::conda_backend::Remote<SlurmBackend>>, String> {
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
             iunctus on a node of the SLURM cluster, like its login node"
        ));
    }
    let runtime = workflow_graph::runtime().map_err(|error| error.to_string())?;
    runtime
        .block_on(backend.reconcile())
        .map_err(|error| error.to_string())?;
    // The nodes read the inputs from the store, so they are copied there.
    let scratch = backend.scratch.clone();
    let backend = crate::conda_backend::Remote::new(backend, store.clone(), &scratch, run, setup);
    let runner = Runner::new(store, backend, run, crate::runner_settings(storage, true));
    Ok(RunnerExecutor::new(runner, default_image))
}
