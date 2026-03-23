//! The interface between a language and the engine.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::{Language, Outcome, Task};

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
