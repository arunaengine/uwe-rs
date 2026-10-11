//! Runs task attempts as SLURM jobs using configured container runtimes.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod backend;
mod images;
mod queue;
mod remote;
mod script;

pub use backend::{SlurmBackend, TOOLS, executable};
pub use images::{file_name, pull_script};
pub use remote::{Helper, NodeStaging, secret_text};
pub use script::{
    CONTAINER_EXIT, Container, LOG, PullResources, SCRIPT, TIMED_OUT, container_script, job_script,
    pull_options, sbatch_options,
};
