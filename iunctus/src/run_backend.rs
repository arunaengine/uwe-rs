//! Backend configuration shared by the CLI and embedding API.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::arguments::Arguments;
use crate::repeat::Pinned;
use crate::setup::Setup;
use crate::{Failure, resources};
use workflow_graph::Executor;

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
