//! Markers: the points of a graph where the tasks that follow depend on data, so that a run
//! with real data can have more or fewer tasks there than a simulated run (decision 21).
//! Drivers report what they notice while they plan ([`Note`]); the engine turns the notes into
//! markers on the tasks whose outputs they concern.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::TaskId;

/// What every simulated graph and run says about itself.
pub const SIMULATION: &str = "This is a simulation: no task ran, and the outputs are made-up \
                              files. A run with real data refines this graph.";

/// What the renderers write in the node that follows a marked task or step of a simulated
/// graph.
pub const MORE_TASKS: &str = "more tasks may follow from real data";

/// A point where the tasks that follow a task depend on its outputs: their content or their
/// number. In a simulated graph the outputs are made up, so a run with real data can have
/// more or fewer tasks after this point.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub kind: MarkerKind,
    /// Why, in one short sentence, like "the workflow read `samples.csv`, an output of this
    /// task, while it planned the next tasks".
    pub reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkerKind {
    /// The workflow read the content of an output of the task, or listed an output folder,
    /// while it planned the next tasks.
    Content,
    /// An output pattern with a wildcard matched files that each started tasks of their own,
    /// so the number of files decides the number of tasks.
    Wildcard,
    /// A Snakemake checkpoint: Snakemake plans the workflow again after it.
    Checkpoint,
}

/// What a driver noticed while it planned tasks. The engine asks for notes after `start` and
/// after each `complete` ([`crate::Driver::notes`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Note {
    /// The workflow read the content of this file, or listed this folder, while it planned
    /// tasks. The path is where the file is now, like the location of a task output. Reads of
    /// files that no task made, like a sample sheet of the user, mark nothing.
    Read(PathBuf),
    /// Each file like this one started tasks of its own, as a Nextflow `flatten` or a CWL
    /// scatter over files does, so the number of such files decides the number of tasks. It
    /// marks the task that made the file, when an output pattern with a wildcard matched it.
    Spread(PathBuf),
    /// A marker that only the driver can find, like a Snakemake checkpoint or an Argo
    /// `withParam` over the outputs of a task.
    Mark(TaskId, Marker),
}

impl Marker {
    pub fn new(kind: MarkerKind, reason: impl Into<String>) -> Self {
        Marker {
            kind,
            reason: reason.into(),
        }
    }
}
