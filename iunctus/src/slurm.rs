//! The SLURM executor: each task runs as a SLURM batch job through the task runner. The nodes
//! must see the store and the scratch directories at the same paths, through a shared file
//! system, as the cache directory in a home folder usually is.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use content_store::Store;
use executor::{EngineCommand, EngineKind};
use slurm_executor::{Container, SlurmBackend};
use storage::Storage;
use task_runner::{Runner, RunnerExecutor, Settings};

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

/// An executor that submits each task as a batch job with the `sbatch` options `options`,
/// whose job runs the task with `container`. Tasks without an image get `default_image`.
pub fn executor(
    store: Store,
    scratch: &Path,
    run: &str,
    default_image: Option<String>,
    storage: Storage,
    container: Container,
    options: Vec<String>,
) -> Result<RunnerExecutor<SlurmBackend>, String> {
    let mut backend = SlurmBackend::new(scratch.join("slurm"), container);
    backend.options = options;
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
    // The nodes read the inputs from the store, so they are copied there.
    let settings = Settings {
        downloads: crate::cache_directory().join("downloads"),
        import_inputs: true,
        storage,
    };
    let runner = Runner::new(store, backend, run, settings);
    Ok(RunnerExecutor::new(runner, default_image))
}
