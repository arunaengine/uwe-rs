//! The interface between a language and the engine.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;
use std::sync::Arc;

use crate::{Delivery, Files, Inputs, Language, Note, Outcome, Task};

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
    /// Where the results go of languages that have no place of their own for them, from
    /// `-output-dir`: the files of the CWL output object, as `cwltool --outdir` puts them, the
    /// current directory by default; Argo's output artifacts, `results` by default; the
    /// Nextflow `output` block, `results` by default.
    pub output_dir: Option<PathBuf>,
    /// Run the stubs of tasks instead of their scripts, where the language has stubs.
    pub stub: bool,
    /// Whether the run is simulated: tasks make up their files (decision 21). Drivers then take
    /// input files that no task makes and that do not exist as present, so that a workflow
    /// plans all its tasks without its data, and log the files they assume. Other workarounds
    /// for made-up files hold only in simulated runs too, like placeholders for missing
    /// values; a real run follows the reference tool.
    pub simulated: bool,
    /// Whether the run goes on after a task failed, with every task that does not depend on
    /// the failed one, as Snakemake's `--keep-going` and cwltool's `--on-error continue` do;
    /// the run still fails at its end. Without it, each language stops as its engine does:
    /// no more tasks start, and the running ones end first, except where the language stops
    /// them at once, like Nextflow's default error strategy `terminate`.
    pub keep_going: bool,
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

    /// The files to deliver to where the language expects its results, like the files of a
    /// Nextflow `publishDir`, since the last call. The engine asks after `start`, each
    /// `complete` and `finish`, and a real executor delivers them from its store.
    fn deliveries(&mut self) -> Vec<Delivery> {
        Vec::new()
    }

    /// What the driver noticed since the last call while it planned tasks, like reads of task
    /// outputs, for the markers of the graph. The engine asks after `start` and after each
    /// `complete`. The default notices nothing.
    fn notes(&mut self) -> Vec<Note> {
        Vec::new()
    }

    /// The inputs of the workflow as one JSON Schema, with the values of this run (decision
    /// 24), which the binary checks before any task starts. A driver may load the workflow
    /// here and keep it for [`Driver::start`]. The default knows no inputs.
    fn inputs(&mut self) -> Result<Option<Inputs>, DriverError> {
        Ok(None)
    }

    /// The files that the tasks make, where the executor keeps them. A driver that reads the
    /// content of task outputs, like a Nextflow `.command.env`, reads it through
    /// [`Files::local`], so that a store elsewhere works too ([`crate::files::read`]). The
    /// engine gives them before `start`; a simulated run has none, and its outputs are local
    /// files. The default keeps nothing.
    fn use_files(&mut self, _files: Arc<dyn Files>) {}

    /// The outputs of the workflow as one JSON Schema of the results of a run, from what the
    /// workflow declares, so that one workflow can feed another (decision 24). Like
    /// [`Driver::inputs`], it runs no task. The default knows no outputs.
    fn outputs(&mut self) -> Result<Option<serde_json::Value>, DriverError> {
        Ok(None)
    }
}
