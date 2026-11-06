//! Runs task attempts as SLURM jobs, or as jobs of other schedulers that uwe drives with their
//! tools, using configured container runtimes.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod backend;
pub mod grid;
mod images;
mod queue;
mod remote;
mod script;

pub use backend::{SlurmBackend, TOOLS, executable};
pub use images::{archive_id, file_name, pull_script};
pub use remote::{Helper, NodeStaging, secret_text};
pub use script::{
    CONTAINER_EXIT, Container, LOG, PullResources, SCRIPT, TIMED_OUT, container_script, job_script,
    per_cpu_memory, pull_options, sbatch_options,
};
