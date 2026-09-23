//! Embeddable application services for configured iunctus workflow drivers and executors.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod application;
pub mod forms;

mod arguments;
mod changes;
mod check;
mod clean;
mod conda;
mod conda_backend;
mod detect;
mod failure;
mod fetched;
mod inputs;
mod insight;
mod kubernetes;
mod local;
mod logging;
mod machine;
mod nextflow;
mod order;
mod page;
mod preview;
mod progress;
mod record;
mod repeat;
mod report;
mod resources;
mod resume;
mod revision;
pub mod run;
mod run_backend;
mod run_driver;
mod run_support;
mod runs;
pub mod setup;
mod slurm;
mod sources;
mod terminal;
mod tes;
mod tracked;

#[doc(hidden)]
pub mod cli;

pub use arguments::{Arguments as RunSettings, Resume};
pub use failure::Failure;
pub(crate) use failure::failure;
pub(crate) use run::{Ran, close, execute};
pub(crate) use run_backend::{executor, network_rule, recorded_workflow};
pub(crate) use run_driver::{driver, launch, target_and_language};
pub(crate) use run_support::{
    cache_directory, finish, run_id, runner_settings, stop_on_interrupt, use_cache_folder,
};
