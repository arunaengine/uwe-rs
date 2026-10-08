//! The interface between a language and the engine.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;
use std::sync::Arc;

use crate::{
    CancellationToken, Delivery, ExecuteError, Files, Inputs, Language, Note, Outcome, Task, TaskId,
};

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
    /// Snakemake rules or files to run up to, including their dependencies.
    pub until: Vec<String>,
    /// Snakemake rules or files to omit, together with everything downstream.
    pub omit_from: Vec<String>,
    /// Snakemake rules or files to rerun, together with everything downstream.
    pub forcerun: Vec<String>,
    /// Rerun every Snakemake job needed for the selected targets.
    pub forceall: bool,
    /// Where the run keeps files of its own, like downloads.
    pub scratch: PathBuf,
    /// Selects language result destinations from `-output-dir`, retaining each language
    /// default.
    /// See `docs/design/graph.md` for the full contract.
    pub output_dir: Option<PathBuf>,
    /// Run the stubs of tasks instead of their scripts, where the language has stubs.
    pub stub: bool,
    /// Whether the run is simulated: tasks make up their files (decision 21).
    /// See `docs/design/graph.md` for the full contract.
    pub simulated: bool,
    /// Continues independent tasks after failure when enabled; the final run still fails.
    /// See `docs/design/graph.md` for the full contract.
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

    /// The next time the state machine needs to advance without a task outcome.
    fn wake_at(&self) -> Option<std::time::Instant> {
        None
    }

    /// Advances timers and returns tasks that became ready.
    fn wake(&mut self) -> Result<Vec<Task>, DriverError> {
        Ok(Vec::new())
    }

    /// Tasks whose individual attempts must stop, without stopping the run.
    fn cancellations(&mut self) -> Vec<TaskId> {
        Vec::new()
    }

    /// Whether execution failures should become outcomes for language retry decisions.
    fn handles_error(&self, _task: TaskId, _error: &ExecuteError) -> bool {
        false
    }

    /// Consumes explicit output-validation acceptance for exactly this task and attempt after
    /// [`Driver::complete`].
    /// See `docs/design/graph.md` for the full contract.
    fn take_acceptance(&mut self, _task: TaskId, _attempt: u32) -> bool {
        false
    }

    /// Ends the run: an error when the workflow failed or waits for something that will not
    /// come.
    fn finish(&mut self) -> Result<(), DriverError>;

    /// Ends a failed run after the attempts in flight have been cleaned up. The original
    /// execution error remains the cause of the run's failure.
    fn failed(&mut self, _error: &ExecuteError) -> Result<(), DriverError> {
        Ok(())
    }

    /// The files to deliver to where the language expects its results, like the files of a
    /// Nextflow `publishDir`, since the last call. The engine asks after `start`, each
    /// `complete` and `finish`, and a real executor delivers them from its store.
    fn deliveries(&mut self) -> Vec<Delivery> {
        Vec::new()
    }

    /// Acknowledges a successful delivery by an executor's files provider.
    fn delivered(&mut self, _delivery: &Delivery) -> Result<(), DriverError> {
        Ok(())
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

    /// Applies normalized input values before [`Driver::start`]. The default accepts values
    /// only when normalization changed nothing, so a driver never silently executes other
    /// values than the ones that were checked.
    fn use_inputs(&mut self, values: serde_json::Value) -> Result<(), DriverError> {
        let same = self.inputs()?.is_some_and(|inputs| inputs.values == values);
        if same {
            Ok(())
        } else {
            Err(DriverError::new(format!(
                "the {} driver cannot apply normalized input values",
                self.language()
            )))
        }
    }

    /// The files that the tasks make, where the executor keeps them.
    /// See `docs/design/graph.md` for the full contract.
    fn use_files(&mut self, _files: Arc<dyn Files>) {}

    /// Supplies cancellation to drivers that evaluate code while planning.
    fn use_cancel(&mut self, _cancel: CancellationToken) {}

    /// The outputs of the workflow as one JSON Schema of the results of a run, from what the
    /// workflow declares, so that one workflow can feed another (decision 24). Like
    /// [`Driver::inputs`], it runs no task. The default knows no outputs.
    fn outputs(&mut self) -> Result<Option<serde_json::Value>, DriverError> {
        Ok(None)
    }

    /// During draining, running tasks finish and report outcomes, while all new admissions
    /// stop.
    /// See `docs/design/graph.md` for the full contract.
    fn draining(&self) -> bool {
        false
    }
}
