//! The language-neutral side of a run. Every language driver (Nextflow, Snakemake, CWL, Argo)
//! turns its program into concrete [`Task`]s; [`execute`] hands them to an [`Executor`] and
//! records the [`ExecutionGraph`] of the run: every task that ran, and which task produced the
//! files another one read.
//!
//! The graph grows while the run goes on, because Nextflow channels, Snakemake checkpoints,
//! CWL scatter over outputs and Argo `withParam` only know their tasks at run time.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod driver;
mod graph;
mod task;

pub use driver::{Driver, DriverError};
pub use graph::{Edge, EdgeKind, ExecutionGraph, Node};
pub use task::{Input, Language, Outcome, OutputFile, Resources, Task, TaskId};
