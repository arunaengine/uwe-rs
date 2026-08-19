//! The language-neutral side of a run. Every language driver (Nextflow, Snakemake, CWL, Argo)
//! turns its program into concrete [`Task`]s; [`execute`] hands them to an [`Executor`] and
//! records the [`ExecutionGraph`] of the run: every task that ran, and which task produced the
//! files another one read.
//!
//! The graph grows while the run goes on, because Nextflow channels, Snakemake checkpoints,
//! CWL scatter over outputs and Argo `withParam` only know their tasks at run time.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod attempts;
mod driver;
mod engine;
mod events;
mod executor;
pub mod files;
mod graph;
mod inputs;
pub mod layout;
mod marker;
mod options;
mod pending;
mod queue;
mod recorder;
pub mod render;
pub mod simulator;
mod steps;
mod task;

pub use driver::{Driver, DriverError, Launch};
pub use engine::{Engine, ExecuteError, Execution, execute, execute_with};
pub use events::{Event, Observer};
pub use executor::{Attempt, CancellationToken, Executor, Running, runtime};
pub use files::{Delivery, DeliveryMode, Files, LocalFiles};
pub use graph::{Edge, EdgeKind, ExecutionGraph, Node};
pub use inputs::Inputs;
pub use marker::{MORE_TASKS, Marker, MarkerKind, Note, SIMULATION};
pub use options::{Order, RunOptions};
pub use simulator::Simulator;
pub use steps::{StepEdge, StepGraph, StepNode};
pub use task::{
    Container, ContainerOutcome, Input, Language, Network, Outcome, OutputFile,
    ReproducibilityCheck, Resources, Reuse, Task, TaskId, Usage, Verdict, Writable,
};
