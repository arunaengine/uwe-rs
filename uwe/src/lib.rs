//! Embeddable application services for configured uwe workflow drivers and executors.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod application;
pub mod catalogue;
pub mod forms;
pub mod hosted;

mod arguments;
mod aws_batch;
mod azure_batch;
mod changes;
mod check;
mod clean;
mod conda;
mod conda_backend;
mod detect;
mod failure;
mod fetched;
mod flyte;
mod git;
mod google_batch;
mod grid;
mod inputs;
mod insight;
mod kubernetes;
mod kuberun;
mod local;
mod logging;
mod machine;
mod nextflow;
mod nomad;
mod order;
mod page;
mod preview;
mod progress;
mod record;
mod repeat;
mod report;
mod repository;
mod resources;
mod resume;
mod revision;
pub mod run;
mod runs;
mod scopes;
pub mod setup;
mod slurm;
mod sources;
mod staging;
mod terminal;
mod tes;
mod tracked;

#[doc(hidden)]
pub mod cli;

pub use arguments::Resume;
pub use failure::Failure;
pub(crate) use failure::failure;
pub(crate) use run::backend::{executor, network_rule, recorded_workflow};
pub(crate) use run::driver::{driver, launch, target_and_language};
pub(crate) use run::support::{
    cache_directory, cwl_archives, finish, run_id, runner_settings, stop_on_interrupt,
    use_cache_folder,
};
pub(crate) use run::{Ran, close, execute};
pub use run::{Run, RunRequest};
pub use workflow_graph::CancellationToken;
