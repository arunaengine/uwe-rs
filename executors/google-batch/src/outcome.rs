//! How an ended Google Batch job becomes the result of its task: the exit code that the wrapper
//! recorded and the job's own, Google's reserved exit codes, and notes for time and memory.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;

use executor::wrapper::{MANIFEST, STDERR, TIMED_OUT};
use executor::{CONTROL, ExecutorError, Finished};
use serde_json::Value;
use task_model::TaskSpec;
use tokio::io::AsyncWriteExt;

/// The exit codes that Google Batch gives a task that its VM ended, from Google's
/// troubleshooting page.
const RESERVED: [(i32, &str); 6] = [
    (50001, "its spot VM was preempted"),
    (50002, "its VM stopped reporting to Google Batch"),
    (50003, "its VM rebooted"),
    (50004, "its VM and the task stopped answering"),
    (50005, "it ran longer than its maximum run time"),
    (50006, "its VM was recreated"),
];

/// The first exit code that Google Batch reserves.
const LEAST_RESERVED: i32 = 50000;

/// How a job ended.
#[derive(Debug, PartialEq, Eq)]
pub struct Ended {
    pub succeeded: bool,
    /// The exit code of its task, when it ran.
    pub exit_code: Option<i32>,
    /// The description of its last status event.
    pub description: String,
}

/// The end of a job of state `SUCCEEDED` or `FAILED`; `None` while it runs.
pub fn ended(job: &Value) -> Option<Ended> {
    let succeeded = match job["status"]["state"].as_str()? {
        "SUCCEEDED" => true,
        "FAILED" => false,
        _ => return None,
    };
    let events = job["status"]["statusEvents"].as_array();
    let events = events.map(Vec::as_slice).unwrap_or_default();
    let description = (events.last())
        .and_then(|event| event["description"].as_str())
        .unwrap_or_default()
        .to_owned();
    Some(Ended {
        succeeded,
        exit_code: exit_code(events),
        description,
    })
}

/// The exit code of the latest status event that has one, by event time.
pub fn exit_code(events: &[Value]) -> Option<i32> {
    let coded = events.iter().filter_map(|event| {
        let code = event["taskExecution"]["exitCode"].as_i64()?;
        let time = event["eventTime"].as_str().unwrap_or_default();
        Some((time.parse::<jiff::Timestamp>().ok(), code))
    });
    // An event without a readable time counts as the latest of those before it.
    let latest = coded.reduce(|latest, next| match (latest.0, next.0) {
        (Some(was), Some(now)) if now < was => latest,
        _ => next,
    });
    latest.and_then(|(_, code)| i32::try_from(code).ok())
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

/// The exit code of the task, and a note for its standard error; a failure of the VM or of
/// Google Batch is an error that another attempt may not have.
fn outcome(
    recorded: Option<i32>,
    ended: &Ended,
    time: Option<u64>,
) -> Result<(i32, Option<String>), ExecutorError> {
    let description = &ended.description;
    match (ended.exit_code, recorded) {
        (Some(TIMED_OUT | 50005), recorded) if time.is_some() && recorded != Some(TIMED_OUT) => {
            let time = time.unwrap_or_default();
            let note = format!("uwe: the task ran longer than its time of {time} seconds\n");
            Ok((143, Some(note)))
        }
        // The command ends with the code of the task, which the wrapper recorded first.
        (_, Some(recorded)) if ended.succeeded || ended.exit_code == Some(recorded) => {
            Ok((recorded, None))
        }
        (Some(code), _) if code >= LEAST_RESERVED => {
            let meaning = (RESERVED.iter().find(|(reserved, _)| *reserved == code))
                .map_or("Google Batch ended it", |(_, meaning)| meaning);
            Err(machine(format!(
                "the task did not end: {meaning} (exit code {code}): {description}"
            )))
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
                "the Google Batch job could not run the task ({status}): {description}"
            )))
        }
    }
}

pub fn machine(message: impl Into<String>) -> ExecutorError {
    ExecutorError::Engine {
        engine: "Google Batch".into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn ended(succeeded: bool, exit_code: Option<i32>) -> Ended {
        Ended {
            succeeded,
            exit_code,
            description: "Task state is updated".into(),
        }
    }

    #[test]
    fn joins_job_outcomes() {
        assert_eq!(
            outcome(Some(0), &ended(true, Some(0)), None).ok(),
            Some((0, None))
        );
        assert_eq!(
            outcome(Some(3), &ended(false, Some(3)), None).ok(),
            Some((3, None))
        );
        // A success with a reserved code takes the code that the wrapper recorded.
        assert_eq!(
            outcome(Some(0), &ended(true, Some(50002)), None).ok(),
            Some((0, None))
        );
        let timed = outcome(None, &ended(false, Some(50005)), Some(60));
        assert_eq!(timed.ok().map(|(code, _)| code), Some(143));
        let memory = outcome(None, &ended(false, Some(137)), None).expect("an outcome");
        assert!(memory.1.is_some_and(|note| note.contains("lack of memory")));
        for code in [Some(50001), Some(125), None] {
            let lost = outcome(None, &ended(false, code), None);
            let Err(ExecutorError::Engine { engine, message }) = lost else {
                panic!("a failure of the VM: {lost:?}");
            };
            assert_eq!(engine, "Google Batch");
            if code == Some(50001) {
                assert!(message.contains("preempted"), "{message}");
            }
        }
    }

    #[test]
    fn reads_job_ends() {
        let job = json!({"status": {"state": "FAILED", "statusEvents": [
            {"eventTime": "2026-11-06T10:00:02.5Z", "taskExecution": {"exitCode": 1}},
            {"eventTime": "2026-11-06T10:00:01Z", "taskExecution": {"exitCode": 50001}},
            {"eventTime": "2026-11-06T10:00:03Z", "description": "Job state is set to FAILED"},
        ]}});
        let end = super::ended(&job).expect("an end");
        assert_eq!(end.exit_code, Some(1));
        assert!(!end.succeeded);
        assert_eq!(end.description, "Job state is set to FAILED");
        let running = json!({"status": {"state": "RUNNING"}});
        assert_eq!(super::ended(&running), None);
    }
}
