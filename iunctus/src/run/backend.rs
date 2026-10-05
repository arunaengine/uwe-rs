//! Backend configuration shared by the CLI and embedding API.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::arguments::Arguments;
use crate::repeat::Pinned;
use crate::setup::Setup;
use crate::{
    Failure, cache_directory, failure, kubernetes, local, preview, resources, runner_settings,
    runs, slurm, tes,
};
use workflow_graph::{Executor, Simulator};

/// A real executor with the settings of the command line: its line per task, whether the cache
/// answers tasks, the share of cached tasks that run again for the reproducibility check, the
/// images that a repeat of a run pins, and which tasks get the network.
pub(crate) fn real<B: executor::Backend>(
    mut executor: task_runner::RunnerExecutor<B>,
    arguments: &Arguments,
    setup: &Setup,
    verbose: bool,
    reuse: bool,
    pinned: &Pinned,
    measured: &resources::Measures,
) -> Result<Box<dyn Executor>, Failure> {
    if pinned
        .images
        .keys()
        .any(|image| image.starts_with("conda-env:"))
    {
        return Err(Failure::setup(
            "iunctus: repeating historical Conda image pins is unsupported; run the workflow with current explicit Conda preparation inputs",
        ));
    }
    executor.verbose = verbose;
    executor.measured = measured.clone();
    executor.right_sizing = !arguments.no_right_sizing;
    executor.together = !arguments.no_grouping;
    executor.reuse = reuse;
    executor.check_reproducibility(arguments.check_reproducibility.unwrap_or(0.0));
    executor.pin_images(&pinned.images);
    executor.network = network_rule(arguments, setup);
    Ok(Box::new(executor))
}

/// Which tasks get the network: `[network]` of the config file, with `-no-network` for the
/// tasks whose workflow does not say.
pub(crate) fn network_rule(arguments: &Arguments, setup: &Setup) -> task_runner::NetworkRule {
    task_runner::NetworkRule {
        none_by_default: arguments.no_network || setup.network.none_by_default,
        ..setup.network.clone()
    }
}

/// An error when the run asks for tasks without network and its executor cannot cut a task
/// off the network yet: TES, Kubernetes, and SLURM with Singularity. It comes
/// before the executor starts, so that it is the first thing the user reads.
pub(crate) fn offline_possible(arguments: &Arguments, setup: &Setup) -> Result<(), Failure> {
    if !network_rule(arguments, setup).none_by_default {
        return Ok(());
    }
    let named = (arguments.slurm_container.as_deref())
        .or(setup.executors.slurm_container.as_deref())
        .unwrap_or("apptainer");
    let why = match arguments.executor.as_deref() {
        Some("tes") => "a TES server has no setting that cuts a task off the network",
        Some("kubernetes") => {
            "the Kubernetes executor makes no network policy that cuts a task off the network"
        }
        Some("slurm") if named == "singularity" => {
            "Singularity cuts a task off the network only where the site allows it, which \
             iunctus does not ask for (`-slurm-container apptainer`, `podman` or `docker` can)"
        }
        _ => return Ok(()),
    };
    Err(Failure::usage(format!(
        "iunctus: {why}, so tasks cannot run without network there yet; leave out \
         `-no-network` and `default = \"none\"` of `[network]`, or use `-executor local`"
    )))
}

/// Builds the executor selected by `-executor`, defaulting to local containers.
/// Without `reuse`, real executors bypass cached results, including during replay.
/// See docs/design/executors.md for backends and scratch paths.
pub(crate) fn executor(
    arguments: &Arguments,
    setup: &Setup,
    run: &str,
    verbose: bool,
    reuse: bool,
    pinned: &Pinned,
    measured: &resources::Measures,
) -> Result<Box<dyn Executor>, Failure> {
    let cache = cache_directory();
    let scratch = cache.join("scratch");
    offline_possible(arguments, setup)?;
    match arguments.executor.as_deref().unwrap_or("local") {
        "simulator" if arguments.check_reproducibility.is_some() => Err(Failure::usage(
            "iunctus: the reproducibility check runs cached tasks again, so it needs a real \
             executor, like `-executor local`",
        )),
        "simulator" => {
            let mut simulator = Simulator::new(scratch.join("simulator").join(run));
            simulator.verbose = verbose;
            Ok(previewed(simulator, arguments, setup))
        }
        "local" => {
            let default_image = arguments.default_image.clone();
            let store = setup.store(arguments)?;
            let storage = setup.storage.clone();
            let engine = setup.engine();
            let mut executor = local::executor(
                store,
                &scratch,
                run,
                default_image,
                storage,
                &engine,
                setup.conda.clone(),
            )
            .map_err(|message| Failure::setup(format!("iunctus: {message}")))?;
            executor.priorities = std::sync::Arc::new(chains(arguments));
            real(executor, arguments, setup, verbose, reuse, pinned, measured)
        }
        "tes" => {
            let default_image = arguments.default_image.clone();
            let settings = &setup.executors;
            let server = (arguments.tes_server.as_deref())
                .or(settings.tes_server.as_deref())
                .unwrap_or(tes::DEFAULT_SERVER);
            let token = match std::env::var("IUNCTUS_TES_TOKEN") {
                Ok(token) if !token.is_empty() => Some(token),
                _ => (settings.tes_token.as_ref().map(|secret| secret.reveal()))
                    .transpose()
                    .map_err(|error| Failure::setup(format!("iunctus: {error}")))?,
            };
            let store = setup.store(arguments)?;
            let storage = setup.storage.clone();
            let place = tes::Place {
                server,
                token,
                settings: &settings.tes,
                sites: &settings.tes_sites,
                scratch: &scratch,
            };
            let executor = tes::executor(store, place, run, default_image, storage)
                .map_err(|message| Failure::setup(format!("iunctus: {message}")))?;
            real(executor, arguments, setup, verbose, reuse, pinned, measured)
        }
        "slurm" => {
            let default_image = arguments.default_image.clone();
            let settings = &setup.executors;
            let named =
                (arguments.slurm_container.as_deref()).or(settings.slurm_container.as_deref());
            let container = slurm::container(named)
                .map_err(|message| Failure::usage(format!("iunctus: {message}")))?;
            let options = match arguments.slurm_options.as_deref() {
                Some(options) => options.split_whitespace().map(str::to_owned).collect(),
                None => settings.slurm_options.clone().unwrap_or_default(),
            };
            let store = setup.store(arguments)?;
            let storage = setup.storage.clone();
            let backend = slurm::backend(&scratch, container, options, settings.slurm_pull);
            let executor = slurm::executor(store, backend, run, default_image, storage)
                .map_err(|message| Failure::setup(format!("iunctus: {message}")))?;
            real(executor, arguments, setup, verbose, reuse, pinned, measured)
        }
        "kubernetes" => {
            let default_image = arguments.default_image.clone();
            let settings = &setup.executors;
            let kubeconfig = (arguments.kubeconfig.as_deref()).or(settings.kubeconfig.as_deref());
            let namespace = (arguments.kubernetes_namespace.clone())
                .or_else(|| settings.kubernetes_namespace.clone());
            let store = setup.store(arguments)?;
            let storage = setup.storage.clone();
            let executor = kubernetes::executor(
                store,
                &scratch,
                run,
                default_image,
                storage,
                kubeconfig,
                namespace,
            )
            .map_err(|message| Failure::setup(format!("iunctus: {message}")))?;
            real(executor, arguments, setup, verbose, reuse, pinned, measured)
        }
        other => Err(Failure::usage(format!(
            "iunctus: unknown executor `{other}`; use `simulator`, `local`, `tes`, `slurm` or \
             `kubernetes`"
        ))),
    }
}

/// For each step of the workflow, the time of the longest chain of work that follows from it
/// in its earlier real runs, so that the tasks on the longest remaining chain start first when
/// tasks wait for the machine. Empty before the first real run, or without a run database.
fn chains(arguments: &Arguments) -> std::collections::HashMap<String, u64> {
    let Some(workflow) = recorded_workflow(arguments) else {
        return Default::default();
    };
    (runs::open(arguments).ok())
        .and_then(|database| database.chains(&workflow).ok())
        .unwrap_or_default()
}

/// The workflow as the run database records it: the target as an absolute path.
pub(crate) fn recorded_workflow(arguments: &Arguments) -> Option<String> {
    let target = arguments.target.as_ref()?;
    let workflow = std::path::absolute(target).unwrap_or_else(|_| target.clone());
    Some(workflow.to_string_lossy().into_owned())
}

/// The simulator with the preview of the cache: tasks whose results the store holds, as a real
/// run would find them, come from there (see [`preview`]). Without a store or a run database
/// to read, every task is simulated.
fn previewed(simulator: Simulator, arguments: &Arguments, setup: &Setup) -> Box<dyn Executor> {
    // A real run without the cache would run every task.
    if arguments.no_cache {
        return Box::new(simulator);
    }
    let pins = runs::open(arguments).and_then(|database| {
        (database.pins()).map_err(|error| failure(format!("iunctus: {error}")))
    });
    let (Ok(pins), Ok(store)) = (pins, setup.store(arguments)) else {
        return Box::new(simulator);
    };
    let settings = runner_settings(setup.storage.clone(), false);
    let lookup = task_runner::Lookup::new(store, settings);
    let default_image = arguments.default_image.clone();
    Box::new(preview::Previewed::new(
        simulator,
        lookup,
        pins,
        default_image,
    ))
}
