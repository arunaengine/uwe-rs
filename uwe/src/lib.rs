//! uwe as a library: runs workflows of any language for a host program, with their input
//! forms, support reports, presets, tool library and the studio of `uwe serve`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod application;
pub mod catalogue;
pub mod forms;
pub mod hosted;
pub mod run;
pub mod serve;
pub mod setup;
pub mod support;
pub mod tools;

#[doc(hidden)]
pub mod cli;

pub use arguments::Resume;
pub use failure::Failure;
pub use run::{Run, RunRequest};
pub use workflow_graph::CancellationToken;

// The command line and the internals that it shares with the embedding modules above.
mod arguments;
mod changes;
mod check;
mod clean;
mod completions;
mod conda;
mod detect;
mod executors;
mod failure;
mod fetched;
mod git;
mod inputs;
mod insight;
mod kuberun;
mod logging;
mod lower;
mod machine;
mod nextflow;
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
mod runs;
mod scaffold;
mod scopes;
mod sources;
mod terminal;
mod tracked;

pub(crate) use failure::failure;
pub(crate) use run::backend::{executor, network_rule, recorded_workflow};
pub(crate) use run::driver::{driver, launch, target_and_language};
pub(crate) use run::support::{
    cache_directory, cwl_archives, finish, journal_folder, run_id, runner_settings,
    stop_on_interrupt, use_cache_folder,
};
pub(crate) use run::{Ran, close, execute};
