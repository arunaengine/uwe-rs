//! How a job service becomes the result of its task: the wrapper's own record first, then the
//! end of its statement, which carries no exit code.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;

use executor::wrapper::{MANIFEST, STDERR, TIMED_OUT};
use executor::{CONTROL, ExecutorError, Finished};
use task_model::TaskSpec;
use tokio::io::AsyncWriteExt;

/// How the statement of a job service ended.
#[derive(Debug, PartialEq, Eq)]
pub struct Ended {
    pub failed: bool,
    /// What Snowflake says about a failure.
    pub message: String,
}

/// What the task left in `root`, with the exit code of the wrapper and the statement together.
pub async fn finish(
    ended: &Ended,
    root: PathBuf,
    time: Option<u64>,
    spec: &TaskSpec,
) -> Result<Finished, ExecutorError> {
    let control = root.join(CONTROL);
    let recorded = executor::containers::recorded(spec, &control);
    let (exit_code, note) = outcome(recorded, ended, time)?;
    if let Some(note) = note {
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(control.join(STDERR))
            .await?;
        file.write_all(note.as_bytes()).await?;
        // A file of Tokio writes in the background until it is flushed.
        file.flush().await?;
    }
    let files = match exit_code {
        0 => {
            let manifest = tokio::fs::read_to_string(control.join(MANIFEST)).await;
            executor::manifest::parse(&manifest.unwrap_or_default())?
        }
        _ => Vec::new(),
    };
    Ok(Finished {
        address: None,
        exit_code: Some(exit_code),
        files,
        usage: executor::Usage::of(&root),
        root,
    })
}

/// The exit code of the task and a note for its standard error; a job service that could not
/// run the task is an error that another attempt may not have.
fn outcome(
    recorded: Option<i32>,
    ended: &Ended,
    time: Option<u64>,
) -> Result<(i32, Option<String>), ExecutorError> {
    let timeout = ended.failed && ended.message.to_ascii_lowercase().contains("timeout");
    match recorded {
        _ if (recorded == Some(TIMED_OUT) || timeout) && time.is_some() => {
            let time = time.unwrap_or_default();
            let note = format!("uwe: the task ran longer than its time of {time} seconds\n");
            Ok((143, Some(note)))
        }
        // The wrapper records the code of the task first, which wins over the statement.
        Some(recorded) => Ok((recorded, None)),
        None => Err(machine(format!(
            "the Snowflake job service could not run the task: {}",
            match ended.failed {
                true => ended.message.as_str(),
                false => "it ended without the record of the task",
            }
        ))),
    }
}

pub fn machine(message: impl Into<String>) -> ExecutorError {
    ExecutorError::Engine {
        engine: "Snowflake".into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ended(failed: bool, message: &str) -> Ended {
        Ended {
            failed,
            message: message.into(),
        }
    }

    #[test]
    fn joins_job_outcomes() {
        let done = outcome(Some(0), &ended(false, ""), None).ok();
        assert_eq!(done, Some((0, None)));
        let failed = ended(true, "Job UWE_1 failed: container exited with 3");
        assert_eq!(outcome(Some(3), &failed, None).ok(), Some((3, None)));
        let timeout = ended(true, "Statement reached its statement or warehouse timeout");
        let timed = outcome(None, &timeout, Some(60)).expect("an outcome");
        assert_eq!(timed.0, 143);
        let lost = outcome(None, &ended(true, "image not found"), None);
        let Err(ExecutorError::Engine { message, .. }) = lost else {
            panic!("a failure of the machine: {lost:?}");
        };
        assert!(message.contains("image not found"), "{message}");
    }
}
