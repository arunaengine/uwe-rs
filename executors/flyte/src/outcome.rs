//! How a Flyte run becomes the result of its task: the wrapper's own record first, then the
//! phase of the run, which carries no exit code.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;

use executor::wrapper::{MANIFEST, STDERR, TIMED_OUT};
use executor::{CONTROL, ExecutorError, Finished};
use task_model::TaskSpec;
use tokio::io::AsyncWriteExt;

use crate::run::{Phase, Seen};

/// What the task left in `root`, with the exit code of the wrapper and the run together.
pub async fn finish(
    seen: &Seen,
    root: PathBuf,
    time: Option<u64>,
    spec: &TaskSpec,
) -> Result<Finished, ExecutorError> {
    let control = root.join(CONTROL);
    let recorded = executor::containers::recorded(spec, &control);
    let (exit_code, note) = outcome(recorded, seen, time)?;
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

/// The exit code of the task and a note for its standard error; a run that could not run the
/// task is an error that another attempt may not have.
fn outcome(
    recorded: Option<i32>,
    seen: &Seen,
    time: Option<u64>,
) -> Result<(i32, Option<String>), ExecutorError> {
    let timed_out = recorded == Some(TIMED_OUT) || seen.phase == Phase::TimedOut;
    match recorded {
        _ if timed_out && time.is_some() => {
            let time = time.unwrap_or_default();
            let note = format!("uwe: the task ran longer than its time of {time} seconds\n");
            Ok((143, Some(note)))
        }
        // The wrapper records the code of the task first, which wins over Flyte's phase.
        Some(recorded) => Ok((recorded, None)),
        None => Err(machine(format!(
            "the Flyte run ended {:?} without running the task: {}",
            seen.phase, seen.message
        ))),
    }
}

pub fn machine(message: impl Into<String>) -> ExecutorError {
    ExecutorError::Engine {
        engine: "Flyte".into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(phase: Phase) -> Seen {
        Seen {
            phase,
            message: "OOMKilled".into(),
            attempt: 1,
        }
    }

    #[test]
    fn joins_run_outcomes() {
        let done = outcome(Some(0), &seen(Phase::Succeeded), None).ok();
        assert_eq!(done, Some((0, None)));
        // A failed run whose wrapper recorded a code keeps the wrapper's code.
        assert_eq!(
            outcome(Some(3), &seen(Phase::Failed), None).ok(),
            Some((3, None))
        );
        let timed = outcome(None, &seen(Phase::TimedOut), Some(60)).expect("an outcome");
        assert_eq!(timed.0, 143);
        let lost = outcome(None, &seen(Phase::Failed), None);
        let Err(ExecutorError::Engine { message, .. }) = lost else {
            panic!("a failure of the machine: {lost:?}");
        };
        assert!(message.contains("OOMKilled"), "{message}");
    }
}
