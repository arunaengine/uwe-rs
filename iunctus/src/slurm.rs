//! The SLURM executor: each task runs as a SLURM batch job through the task runner. The nodes
//! must see the store and the scratch directories at the same paths, through a shared file
//! system, as the cache directory in a home folder usually is.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use content_store::Store;
use executor::{ContainerEngine, EngineKind};
use slurm_executor::{Container, SlurmBackend};
use task_runner::{Runner, RunnerExecutor, Settings};

/// The container runtime of the nodes that `-slurm-container` names: `apptainer` (the
/// default), `singularity`, `podman` or `docker`.
pub fn container(name: Option<&str>) -> Result<Container, String> {
    match name.unwrap_or("apptainer") {
        name @ ("apptainer" | "singularity") => Ok(Container::Apptainer(name.to_owned())),
        "podman" => Ok(Container::Engine(ContainerEngine {
            program: "podman".into(),
            kind: EngineKind::Podman,
            rootless: true,
        })),
        "docker" => Ok(Container::Engine(ContainerEngine::docker("docker"))),
        other => Err(format!(
            "unknown container runtime `{other}` for SLURM; use apptainer, singularity, podman \
             or docker"
        )),
    }
}

/// An executor that submits each task as a batch job with the `sbatch` options `options`,
/// whose job runs the task with `container`. Tasks without an image get `default_image`.
pub fn executor(
    store: &Path,
    scratch: &Path,
    run: &str,
    default_image: Option<String>,
    container: Container,
    options: Vec<String>,
) -> Result<RunnerExecutor<SlurmBackend>, String> {
    let store = Store::local(store).map_err(|error| error.to_string())?;
    let mut backend = SlurmBackend::new(scratch.join("slurm"), container);
    backend.options = options;
    // The nodes read the inputs from the store, so they are copied there.
    let settings = Settings {
        downloads: crate::cache_directory().join("downloads"),
        import_inputs: true,
    };
    let runner = Runner::new(store, backend, run, settings);
    Ok(RunnerExecutor::new(runner, default_image))
}
