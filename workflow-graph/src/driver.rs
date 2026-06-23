//! The interface between a language and the engine.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;

use crate::{Language, Outcome, Task};

/// What a user asks to run, in the terms all languages share. Each language crate exposes
/// `pub fn driver(launch: &Launch) -> Result<Box<dyn Driver>, DriverError>`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Launch {
    /// The workflow file or project directory.
    pub target: PathBuf,
    /// Workflow parameters from the command line, like `--outdir results` or
    /// `--config samples=s.tsv`, as names and text values in the order given.
    pub params: Vec<(String, String)>,
    /// A file of inputs or parameters: a CWL job file, a Nextflow params file, a Snakemake
    /// config file or an Argo parameter file.
    pub inputs: Option<PathBuf>,
    /// Nextflow profiles.
    pub profiles: Vec<String>,
    /// Extra config files, like Nextflow `-c` or Snakemake `--configfile`.
    pub configs: Vec<PathBuf>,
    /// Targets or an entry point: Snakemake target files or rules, the Nextflow `-entry`
    /// workflow, the Argo entrypoint.
    pub targets: Vec<String>,
    /// Where the run keeps files of its own, like downloads.
    pub scratch: PathBuf,
    /// Run the stubs of tasks instead of their scripts, where the language has stubs.
    pub stub: bool,
    /// Treat input files that no task makes and that do not exist as present, so that a
    /// workflow plans all its tasks without its data, as a simulated run does. Drivers log the
    /// files they assume.
    pub assume_inputs: bool,
}

/// Why a driver cannot go on.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct DriverError {
    pub message: String,
    /// Whether the cause is a feature of the language that iunctus does not support yet; the
    /// command then exits with code 33, as CWL runners do.
    pub unsupported: bool,
}

impl DriverError {
    pub fn new(message: impl Into<String>) -> Self {
        DriverError {
            message: message.into(),
            unsupported: false,
        }
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        DriverError {
            message: message.into(),
            unsupported: true,
        }
    }
}

/// A workflow language as a state machine: it asks for tasks, hears how they ended, and asks
/// for more. The engine calls [`Driver::start`] once, [`Driver::complete`] once per task, in the
/// order tasks finish, and [`Driver::finish`] when no task is left.
pub trait Driver {
    fn language(&self) -> Language;

    /// Loads the workflow and returns the tasks that can run first.
    fn start(&mut self) -> Result<Vec<Task>, DriverError>;

    /// Takes the outcome of a task and returns the tasks that can run now.
    fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError>;

    /// Ends the run: an error when the workflow failed or waits for something that will not
    /// come.
    fn finish(&mut self) -> Result<(), DriverError>;
}
