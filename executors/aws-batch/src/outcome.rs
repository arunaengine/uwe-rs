//! How an ended AWS Batch job becomes the result of its task: the exit code that the wrapper
//! recorded and the job's own, its reason, and notes for time and memory.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;

use executor::wrapper::{MANIFEST, STDERR, TIMED_OUT};
use executor::{CONTROL, ExecutorError, Finished};
use serde_json::Value;
use task_model::TaskSpec;
use tokio::io::AsyncWriteExt;

/// How a job ended.
#[derive(Debug, PartialEq, Eq)]
pub struct Ended {
    pub succeeded: bool,
    /// The exit code of its container, when it ran.
    pub exit_code: Option<i32>,
    /// Why AWS Batch ended it, from the job and its last attempt.
    pub reason: String,
    /// The CloudWatch Logs stream of its container.
    pub log_stream: Option<String>,
}

/// Why AWS Batch ended or holds a job: its `statusReason` and the reason of the container of
/// its last attempt, joined with ` - `.
pub fn reason(job: &Value) -> String {
    let attempts = job["attempts"].as_array();
    let container = (attempts.and_then(|attempts| attempts.last()))
        .map(|attempt| &attempt["container"])
        .unwrap_or(&job["container"]);
    let parts = [&job["statusReason"], &container["reason"]];
    let parts: Vec<&str> = parts.iter().filter_map(|part| part.as_str()).collect();
    parts.join(" - ")
}

pub fn exit_code(job: &Value) -> Option<i32> {
    let code = job["container"]["exitCode"].as_i64().or_else(|| {
        let attempts = job["attempts"].as_array()?;
        attempts.last()?["container"]["exitCode"].as_i64()
    });
    code.and_then(|code| i32::try_from(code).ok())
}

pub fn log_stream(job: &Value) -> Option<String> {
    let stream = job["container"]["logStreamName"].as_str().or_else(|| {
        let attempts = job["attempts"].as_array()?;
        attempts.last()?["container"]["logStreamName"].as_str()
    });
    stream.map(str::to_owned)
}

/// What the task left in `root`, with the exit code of the wrapper and the job together.
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

/// The exit code of the task, and a note for its standard error; a failure of the machine or
/// of AWS Batch is an error that another attempt may not have.
fn outcome(
    recorded: Option<i32>,
    ended: &Ended,
    time: Option<u64>,
) -> Result<(i32, Option<String>), ExecutorError> {
    let reason = &ended.reason;
    if reason.contains("CannotPullContainer") && reason.contains("unauthorized") {
        return Err(ExecutorError::MissingImage {
            image: String::new(),
            reason: reason.clone(),
        });
    }
    match (ended.exit_code, recorded) {
        (Some(TIMED_OUT), recorded) if time.is_some() && recorded != Some(TIMED_OUT) => {
            let time = time.unwrap_or_default();
            let note = format!("uwe: the task ran longer than its time of {time} seconds\n");
            Ok((143, Some(note)))
        }
        (Some(status), _) if reason.contains("OutOfMemory") => Ok((
            status,
            Some("uwe: AWS Batch stopped the task for lack of memory\n".to_owned()),
        )),
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
                "the AWS Batch job could not run the task ({status}): {reason}"
            )))
        }
    }
}

pub fn machine(message: impl Into<String>) -> ExecutorError {
    ExecutorError::Engine {
        engine: "AWS Batch".into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn ended(succeeded: bool, exit_code: Option<i32>, reason: &str) -> Ended {
        Ended {
            succeeded,
            exit_code,
            reason: reason.into(),
            log_stream: None,
        }
    }

    #[test]
    fn joins_job_outcomes() {
        assert_eq!(
            outcome(Some(0), &ended(true, Some(0), ""), None).ok(),
            Some((0, None))
        );
        assert_eq!(
            outcome(Some(3), &ended(false, Some(3), ""), None).ok(),
            Some((3, None))
        );
        let timed = outcome(None, &ended(false, Some(TIMED_OUT), ""), Some(60));
        assert_eq!(timed.ok().map(|(code, _)| code), Some(143));
        let memory = ended(false, Some(137), "OutOfMemoryError: Container killed");
        let (code, note) = outcome(None, &memory, None).expect("an outcome");
        assert_eq!(code, 137);
        assert!(note.is_some_and(|note| note.contains("lack of memory")));
        let lost = outcome(
            None,
            &ended(false, None, "Host EC2 (instance i-1) terminated."),
            None,
        );
        assert!(
            matches!(lost, Err(ExecutorError::Engine { .. })),
            "{lost:?}"
        );
        let pull = "CannotPullContainerError: unauthorized: authentication required";
        let pull = outcome(None, &ended(false, None, pull), None);
        assert!(
            matches!(pull, Err(ExecutorError::MissingImage { .. })),
            "{pull:?}"
        );
    }

    #[test]
    fn reads_job_reasons() {
        let job = json!({
            "statusReason": "Essential container in task exited",
            "attempts": [{"container": {"reason": "first"}}, {"container": {"reason": "OutOfMemoryError", "exitCode": 137, "logStreamName": "s/1"}}],
        });
        assert_eq!(
            reason(&job),
            "Essential container in task exited - OutOfMemoryError"
        );
        assert_eq!(exit_code(&job), Some(137));
        assert_eq!(log_stream(&job).as_deref(), Some("s/1"));
    }
}
