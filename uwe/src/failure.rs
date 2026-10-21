//! Formats command failures and classifies their exit codes.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{DriverError, ExecuteError};

/// The exit code of a workflow that failed or cannot run: an error in its files, inputs that
/// do not fit its schema, a task that failed, or a result that cannot be delivered.
pub const FAILED: u8 = 1;

/// The exit code of a command line that uwe does not understand: an unknown command or
/// option, a value that an option does not take, or an option of another command.
pub const USAGE: u8 = 2;

/// The exit code when this machine or its config is not ready: the config file, the container
/// engine, the store, the run database, or what an executor needs, like its server.
pub const SETUP: u8 = 3;

/// The exit code for a feature that a workflow language supports but uwe does not yet, as
/// CWL runners use it for unsupported requirements.
pub const UNSUPPORTED: u8 = 33;

/// The exit code of a run stopped with Ctrl-C, as shells give it.
pub const INTERRUPTED: u8 = 130;

/// What every error line starts with, so that the user knows who speaks.
const PREFIX: &str = "uwe: ";

/// Why a command failed, and its exit code.
#[derive(Debug)]
pub struct Failure {
    pub message: String,
    pub code: u8,
    /// Lines for standard error after the message, like the note of a simulated run.
    pub after: Vec<String>,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.lines().join("\n"))
    }
}

impl std::error::Error for Failure {}

impl Failure {
    /// The workflow failed or cannot run.
    pub fn new(message: impl Into<String>) -> Failure {
        Failure::with_code(message, FAILED)
    }

    /// The command line is wrong.
    pub fn usage(message: impl Into<String>) -> Failure {
        Failure::with_code(message, USAGE)
    }

    /// This machine or its config is not ready.
    pub fn setup(message: impl Into<String>) -> Failure {
        Failure::with_code(message, SETUP)
    }

    /// A command that has said why it failed, like a check that printed its report.
    pub fn said(code: u8) -> Failure {
        Failure::with_code(String::new(), code)
    }

    fn with_code(message: impl Into<String>, code: u8) -> Failure {
        Failure {
            message: message.into(),
            code,
            after: Vec::new(),
        }
    }

    /// The lines that standard error shows: the message in the form of every error, then the
    /// lines after it.
    pub fn lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = self.message.lines().map(shown).collect();
        lines.extend(self.after.iter().cloned());
        lines
    }
}

impl From<DriverError> for Failure {
    fn from(error: DriverError) -> Self {
        let code = if error.unsupported {
            UNSUPPORTED
        } else {
            FAILED
        };
        Failure::with_code(error.message, code)
    }
}

impl From<ExecuteError> for Failure {
    fn from(error: ExecuteError) -> Self {
        let code = match &error {
            ExecuteError::Cancelled => INTERRUPTED,
            // The container engine, the TES server or the cluster failed, not the workflow.
            ExecuteError::Machine { .. } => SETUP,
            error if error.unsupported() => UNSUPPORTED,
            _ => FAILED,
        };
        Failure::with_code(error.to_string(), code)
    }
}

/// A failure of the workflow.
pub fn failure(message: impl Into<String>) -> Failure {
    Failure::new(message)
}

/// A line of an error as uwe shows it: with the prefix of every error, unless it continues
/// the line before it, and without the numbers of system errors, like ` (os error 2)` after
/// `No such file or directory`, which say nothing more.
fn shown(line: &str) -> String {
    let line = without_error_numbers(line);
    if continues(&line) || line.starts_with(PREFIX) {
        return line;
    }
    format!("{PREFIX}{line}")
}

/// Whether a line of a message continues the line before it: it is empty, starts with a space,
/// or is an item of a list, like the `* --input ...` lines of nf-schema.
pub fn continues(line: &str) -> bool {
    line.is_empty()
        || line.starts_with(char::is_whitespace)
        || line.starts_with("* ")
        || line.starts_with("- ")
}

/// The text without ` (os error <number>)`, which uwe never shows.
pub fn without_error_numbers(line: &str) -> String {
    const START: &str = " (os error ";
    let mut shown = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find(START) {
        let after = &rest[at + START.len()..];
        let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 || !after[digits..].starts_with(')') {
            shown.push_str(&rest[..at + START.len()]);
            rest = after;
            continue;
        }
        shown.push_str(&rest[..at]);
        rest = &after[digits + 1..];
    }
    shown.push_str(rest);
    shown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_each_line() {
        let failure = Failure::new(
            "main.nf:3:5: Expected `}`\nuwe: the input `x` is required\n    affected files:\n* \
             --input: missing\n",
        );
        assert_eq!(
            failure.lines(),
            [
                "uwe: main.nf:3:5: Expected `}`",
                "uwe: the input `x` is required",
                "    affected files:",
                "* --input: missing"
            ]
        );
    }

    #[test]
    fn omits_error_numbers() {
        let line = "cannot read job.yml: No such file or directory (os error 2)";
        assert_eq!(
            shown(line),
            "uwe: cannot read job.yml: No such file or directory"
        );
        let line = "a (os error 2) and b (os error x) and c (os error 13)";
        assert_eq!(without_error_numbers(line), "a and b (os error x) and c");
    }

    #[test]
    fn classifies_setup_failures() {
        let error = ExecuteError::Machine {
            task: "A".into(),
            message: "the TES server does not answer".into(),
        };
        assert_eq!(Failure::from(error).code, SETUP);
        let error = DriverError::unsupported("DSL1 scripts are not supported");
        assert_eq!(Failure::from(error).code, UNSUPPORTED);
        assert_eq!(Failure::usage("x").code, USAGE);
    }
}
