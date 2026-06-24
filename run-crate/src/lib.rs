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
mod ids;
mod image;
mod metadata;

use std::path::PathBuf;

use run_database::DatabaseError;

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
