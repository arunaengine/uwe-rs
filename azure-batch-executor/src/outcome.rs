//! How a completed Azure Batch task becomes the result of its task attempt: the exit code that
//! the wrapper recorded and the task's own, its failure, and notes for time and memory.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;

use executor::wrapper::{MANIFEST, STDERR, TIMED_OUT};
use executor::{CONTROL, ExecutorError, Finished};
use serde_json::Value;
use task_model::TaskSpec;
use tokio::io::AsyncWriteExt;

use crate::client::BatchError;

/// How a task ended.
#[derive(Debug, PartialEq, Eq)]
pub struct Ended {
    pub succeeded: bool,
    /// The exit code of its command, when it ran.
    pub exit_code: Option<i32>,
    /// Why Azure Batch failed it: the code and message of its failure.
    pub failure: String,
}

/// The end of a task of state `completed`; `None` while it waits or runs.
pub fn ended(task: &Value) -> Option<Ended> {
    if task["state"].as_str()? != "completed" {
        return None;
    }
    let info = &task["executionInfo"];
    let failure = &info["failureInfo"];
    let parts = [&failure["code"], &failure["message"]];
    let parts: Vec<&str> = parts.iter().filter_map(|part| part.as_str()).collect();
    Some(Ended {
        succeeded: info["result"].as_str() == Some("success"),
        exit_code: (info["exitCode"].as_i64()).and_then(|code| i32::try_from(code).ok()),
        failure: parts.join(": "),
    })
}

/// What the task left in `root`, with the exit code of the wrapper and the task together.
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

/// The exit code of the task, and a note for its standard error; a failure of the node or of
/// Azure Batch is an error that another attempt may not have.
fn outcome(
    recorded: Option<i32>,
    ended: &Ended,
    time: Option<u64>,
) -> Result<(i32, Option<String>), ExecutorError> {
    let failure = &ended.failure;
    // A registry that refuses the image refuses it again, so it is no failure of the node.
    if failure.contains("CannotPullContainer") && failure.contains("unauthorized") {
        return Err(ExecutorError::MissingImage {
            image: String::new(),
            reason: failure.clone(),
        });
    }
    let wall_clock = failure.contains("WallClock") || failure.contains("wall clock");
    match (ended.exit_code, recorded) {
        (code, recorded)
            if time.is_some()
                && (code == Some(TIMED_OUT) || wall_clock)
                && recorded != Some(TIMED_OUT) =>
        {
            let time = time.unwrap_or_default();
            let note = format!("uwe: the task ran longer than its time of {time} seconds\n");
            Ok((143, Some(note)))
        }
        // The command ends with the code of the task, which the wrapper recorded first.
        (_, Some(recorded)) if ended.succeeded || ended.exit_code == Some(recorded) => {
            Ok((recorded, None))
        }
        (Some(status @ 129..=192), _) => Ok((
            status,
            Some(format!(
                "uwe: the container ended with exit code {status}, maybe for lack of memory\n"
            )),
        )),
        (status, _) => {
            let status = status.map_or("no exit code".to_owned(), |code| {
                format!("exit code {code}")
            });
            Err(machine(format!(
                "the Azure Batch task could not run the task ({status}): {failure}"
            )))
        }
    }
}

pub fn machine(message: impl Into<String>) -> ExecutorError {
    ExecutorError::Engine {
        engine: "Azure Batch".into(),
        message: message.into(),
    }
}

/// A failed call: one that may work later is a failure of the machine, others are permanent.
pub fn api(error: BatchError) -> ExecutorError {
    match error.retryable() {
        true => machine(error.to_string()),
        false => ExecutorError::Unsupported(format!("Azure Batch refused a request: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn ended(succeeded: bool, exit_code: Option<i32>, failure: &str) -> Ended {
        Ended {
            succeeded,
            exit_code,
            failure: failure.into(),
        }
    }

    #[test]
    fn joins_task_outcomes() {
        let done = outcome(Some(0), &ended(true, Some(0), ""), None);
        assert_eq!(done.ok(), Some((0, None)));
        let failed = outcome(Some(3), &ended(false, Some(3), "FailureExitCode"), None);
        assert_eq!(failed.ok(), Some((3, None)));
        let timed = outcome(
            None,
            &ended(false, None, "MaxWallClockTimeExceeded: ended"),
            Some(60),
        );
        assert_eq!(timed.ok().map(|(code, _)| code), Some(143));
        let (code, note) = outcome(None, &ended(false, Some(137), ""), None).expect("an outcome");
        assert_eq!(code, 137);
        assert!(note.is_some_and(|note| note.contains("lack of memory")));
        let lost = outcome(None, &ended(false, Some(125), "FailureExitCode"), None);
        assert!(
            matches!(lost, Err(ExecutorError::Engine { .. })),
            "{lost:?}"
        );
        let pull = "ContainerPullError: CannotPullContainer: unauthorized: authentication required";
        let pull = outcome(None, &ended(false, None, pull), None);
        assert!(
            matches!(pull, Err(ExecutorError::MissingImage { .. })),
            "{pull:?}"
        );
    }

    #[test]
    fn reads_task_ends() {
        let task = json!({"state": "completed", "executionInfo": {"exitCode": 1, "result": "failure", "failureInfo": {"category": "usererror", "code": "FailureExitCode", "message": "The task exited with an exit code representing a failure"}}});
        let end = super::ended(&task).expect("an end");
        assert_eq!(end.exit_code, Some(1));
        assert!(!end.succeeded);
        assert!(
            end.failure.starts_with("FailureExitCode: The task exited"),
            "{}",
            end.failure
        );
        assert_eq!(super::ended(&json!({"state": "running"})), None);
    }
}
