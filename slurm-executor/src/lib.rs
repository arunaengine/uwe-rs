//! The SLURM executor: each attempt of a task runs as one batch job, which runs the wrapper in
//! the task's container with Apptainer, Singularity, Podman or Docker on a node. The nodes see
//! the store and the scratch directories at the same paths, through a shared file system.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod backend;
mod images;
mod queue;
mod script;

pub use backend::{SlurmBackend, TOOLS, executable};
pub use images::{file_name, pull_script};
pub use script::{
    CONTAINER_EXIT, Container, LOG, PullResources, SCRIPT, TIMED_OUT, job_script,
    job_script_of_containers, pull_options, sbatch_options,
};
