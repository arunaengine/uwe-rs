//! The SLURM executor: each attempt of a task runs as one batch job, which runs the wrapper in
//! the task's container with Apptainer, Singularity, Podman or Docker on a node. The nodes see
//! the store and the scratch directories at the same paths, through a shared file system.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod backend;
mod queue;
mod script;

pub use backend::SlurmBackend;
pub use script::{CONTAINER_EXIT, Container, LOG, SCRIPT, job_script, sbatch_options};
