//! Runs language-neutral task graphs and records execution events and outcomes.
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
    ReproducibilityCheck, Resources, Reuse, RunAs, Task, TaskId, Usage, Verdict, Writable,
};
