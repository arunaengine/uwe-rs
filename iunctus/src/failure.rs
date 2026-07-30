//! Why a command failed, and its exit code.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{DriverError, ExecuteError};

/// The exit code for a feature that a workflow language supports but iunctus does not yet, as
/// CWL runners use it for unsupported requirements.
pub const UNSUPPORTED: u8 = 33;

/// The exit code of a run stopped with Ctrl-C, as shells give it.
pub const INTERRUPTED: u8 = 130;

/// Why a command failed, and its exit code.
pub struct Failure {
    pub message: String,
    pub code: u8,
    /// Lines for standard error after the message, like the note of a simulated run.
    pub after: Vec<String>,
}

impl From<DriverError> for Failure {
    fn from(error: DriverError) -> Self {
        Failure {
            code: if error.unsupported { UNSUPPORTED } else { 1 },
            message: error.message,
            after: Vec::new(),
        }
    }
}

impl From<ExecuteError> for Failure {
    fn from(error: ExecuteError) -> Self {
        let code = match &error {
            ExecuteError::Cancelled => INTERRUPTED,
            error if error.unsupported() => UNSUPPORTED,
            _ => 1,
        };
        Failure {
            code,
            message: error.to_string(),
            after: Vec::new(),
        }
    }
}

pub fn failure(message: impl Into<String>) -> Failure {
    Failure {
        message: message.into(),
        code: 1,
        after: Vec::new(),
    }
}
