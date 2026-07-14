//! Exports a run of the run database as a Workflow Run RO-Crate of the Provenance Run Crate
//! profile 0.5 (decision 25): the workflow and its language, the inputs and outputs of the run
//! with their SHA-256 digests, and each task as an action with its command, container image,
//! inputs, outputs, start and end. So the crate holds the provenance graph of the run, in the
//! same form for all four languages. A crate of a simulated run says so in its description, in
//! the description of every task, and in a `simulated` property of the crate.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod data;
mod entities;
mod files;
mod formal;
mod ids;
mod image;
mod metadata;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use run_database::{Database, DatabaseError, Run, TaskDetail};
use serde_json::json;

use crate::data::Data;
use crate::metadata::Record;

/// The file name of the metadata of every RO-Crate.
pub const METADATA: &str = "ro-crate-metadata.json";

#[derive(Debug, thiserror::Error)]
pub enum CrateError {
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("RO-Crate: {0}")]
    Io(#[from] std::io::Error),
    #[error("RO-Crate: {0}")]
    Json(#[from] serde_json::Error),
    #[error("RO-Crate: a directory of the run has an invalid manifest: {0}")]
    Tree(task_model::SpecError),
    #[error("the run database has no run `{0}`")]
    UnknownRun(String),
    #[error("RO-Crate: the workflow `{0}` is not a file")]
    Workflow(PathBuf),
}

/// What an export wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    /// The files and directories copied into the crate.
    pub files: usize,
    /// Their size in bytes.
    pub bytes: u64,
    /// The tasks described.
    pub tasks: usize,
}

/// What an export needs besides the run.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// The main workflow file, which the crate holds a copy of: the target of the run, or the
    /// main file of a project directory, like `main.nf` or `workflow/Snakefile`.
    pub workflow: PathBuf,
    /// The license of the crate, as a URL like `https://spdx.org/licenses/CC-BY-4.0`.
    pub license: Option<String>,
}

/// Writes the crate of a run into a directory, creating it when needed.
pub fn export(
    database: &Database,
    run: &str,
    directory: &Path,
    options: &Options,
) -> Result<Summary, CrateError> {
    let Some(record) = database.run(run)? else {
        return Err(CrateError::UnknownRun(run.to_owned()));
    };
    let Some(name) = options
        .workflow
        .file_name()
        .filter(|_| options.workflow.is_file())
    else {
        return Err(CrateError::Workflow(options.workflow.clone()));
    };
    std::fs::create_dir_all(directory)?;
    let workflow = name.to_string_lossy().into_owned();
    std::fs::copy(&options.workflow, directory.join(&workflow))?;
    let mut tasks: Vec<TaskDetail> = Vec::new();
    for row in database.tasks(run)? {
        // A task that never started did not run, so the crate leaves it out.
        if row.attempts > 0
            && let Some(task) = database.task(run, row.id)?
        {
            tasks.push(task);
        }
    }
    let mut data = Data::new(directory);
    let mut placed = HashMap::new();
    for task in &tasks {
        for file in task.inputs.iter().chain(&task.outputs) {
            if !file.location.is_empty() {
                placed.insert(file.location.clone(), data.place(&file.location)?);
                continue;
            }
            // A literal input: its content is in the task description.
            let content = task.description.inputs.iter().find_map(|input| {
                (input.path == file.path)
                    .then_some(input.content.as_deref())
                    .flatten()
            });
            if let (Some(content), Some(digest)) = (content, &file.digest) {
                placed.insert(format!("content:{digest}"), data.place_content(content)?);
            }
        }
    }
    let inputs = database.run_inputs(run)?;
    if let Some(inputs) = &inputs {
        let text = serde_json::to_string_pretty(&inputs.schema)?;
        std::fs::write(directory.join(formal::INPUT_SCHEMA), text + "\n")?;
    }
    let published = ids::time(run_database::now());
    let version = match &record.revision {
        Some(revision) => Some(revision.clone()),
        None => std::fs::read(&options.workflow)
            .ok()
            .map(|bytes| task_model::Digest::sha256(&bytes).to_string()),
    };
    let graph = Record {
        run: &record,
        workflow: &workflow,
        tasks: &tasks,
        placed: &placed,
        published,
        version,
        license: options.license.as_deref(),
        run_inputs: inputs.as_ref(),
    }
    .graph();
    let metadata = json!({
        "@context": [
            "https://w3id.org/ro/crate/1.1/context",
            "https://w3id.org/ro/terms/workflow-run/context"
        ],
        "@graph": graph
    });
    let text = serde_json::to_string_pretty(&metadata)?;
    std::fs::write(directory.join(METADATA), text + "\n")?;
    std::fs::write(
        directory.join(metadata::README),
        readme(&record, &workflow, tasks.len(), inputs.is_some()),
    )?;
    let (files, bytes) = data.totals();
    Ok(Summary {
        files,
        bytes,
        tasks: tasks.len(),
    })
}

/// A short text for people who open the crate folder.
fn readme(run: &Run, workflow: &str, tasks: usize, inputs: bool) -> String {
    let kind = if run.simulated {
        "Simulated run"
    } else {
        "Run"
    };
    let mut text = format!("# {kind} {} of {workflow}\n\n", run.id);
    if run.simulated {
        text.push_str(metadata::SIMULATED);
        text.push_str("\n\n");
    }
    let ended = run.ended.map(ids::time).unwrap_or_else(|| "not yet".into());
    text.push_str(&format!(
        "This folder is a Workflow Run RO-Crate of the Provenance Run Crate profile 0.5, written by \
         iunctus {}.\n\n\
         - `ro-crate-metadata.json` describes the run: the workflow, its steps, and one action per \
         task with its command, container image, inputs, outputs, start and end.\n\
         - `{workflow}` is the workflow that ran.\n{}\
         - `data/` holds each file of the run once, named by its SHA-256 digest; the metadata \
         gives each file its name in the run.\n\n\
         The run {}: started {}, ended {ended}, {tasks} tasks.\n",
        env!("CARGO_PKG_VERSION"),
        match inputs {
            true => format!(
                "- `{}` is the JSON Schema of the inputs of the workflow.\n",
                formal::INPUT_SCHEMA
            ),
            false => String::new(),
        },
        run.state,
        ids::time(run.started),
    ));
    text
}
