//! How a Nomad job ends and becomes the result of its task: the state of its newest allocation,
//! the exit code of its task, the wrapper's own record, and why no node took it.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;

use executor::wrapper::{MANIFEST, STDERR, TIMED_OUT};
use executor::{CONTROL, ExecutorError, Finished};
use serde_json::Value;
use task_model::TaskSpec;
use tokio::io::AsyncWriteExt;

use crate::job::{GROUP, TASK};

/// How a job ended.
#[derive(Debug, PartialEq, Eq)]
pub struct Ended {
    pub failed: bool,
    /// The exit code of the task, from its last `Terminated` event, when it ran.
    pub exit_code: Option<i32>,
    /// The allocation that ran it, for its log.
    pub alloc: String,
    /// What the events of the task say, for errors.
    pub message: String,
    /// Whether Nomad says that the task ran out of memory.
    pub memory: bool,
}

/// What the newest allocation of a job says.
#[derive(Debug, PartialEq, Eq)]
pub enum Seen {
    /// No allocation yet: no node took the job.
    Unplaced,
    Pending,
    Running,
    Ended(Ended),
}

/// The state of the newest of `allocations`.
pub fn seen(allocations: &[Value]) -> Seen {
    let Some(allocation) = allocations.last() else {
        return Seen::Unplaced;
    };
    let state = &allocation["TaskStates"][TASK];
    let status = allocation["ClientStatus"].as_str().unwrap_or_default();
    let dead = state["State"].as_str() == Some("dead");
    let ended = dead || matches!(status, "complete" | "failed" | "lost");
    if !ended {
        return match (status, state["State"].as_str()) {
            ("running", _) | (_, Some("running")) => Seen::Running,
            _ => Seen::Pending,
        };
    }
    let events = state["Events"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    // Nomad leaves the exit code 0 on other events, so only `Terminated` tells it.
    let exit_code = (events.iter().rev())
        .find(|event| event["Type"].as_str() == Some("Terminated"))
        .and_then(|event| event["ExitCode"].as_i64())
        .and_then(|code| i32::try_from(code).ok());
    let texts: Vec<String> = (events.iter())
        .filter_map(|event| {
            let text = (event["DisplayMessage"].as_str())
                .filter(|text| !text.is_empty())
                .or(event["Message"].as_str())
                .unwrap_or_default();
            let kind = event["Type"].as_str().unwrap_or_default();
            (!text.is_empty()).then(|| format!("{kind}: {text}"))
        })
        .collect();
    let message = texts.join("; ");
    let lower = message.to_ascii_lowercase();
    let memory = (events.iter()).any(|event| event["Details"]["oom_killed"] == "true")
        || lower.contains("oom")
        || lower.contains("out of memory");
    Seen::Ended(Ended {
        failed: matches!(status, "failed" | "lost") || state["Failed"] == true,
        exit_code,
        alloc: allocation["ID"].as_str().unwrap_or_default().to_owned(),
        message,
        memory,
    })
}

/// Whether the latest of `evaluations` found no node that could ever take the job: every node
/// it looked at was filtered out, by constraints, class, pool or size. A job that waits only for
/// busy nodes is not.
pub fn unplaceable(evaluations: &[Value]) -> Option<String> {
    let metrics = &evaluations.last()?["FailedTGAllocs"][GROUP];
    if !metrics.is_object() {
        return None;
    }
    let count = |key: &str| metrics[key].as_u64().unwrap_or(0);
    let (evaluated, filtered) = (count("NodesEvaluated"), count("NodesFiltered"));
    (filtered >= evaluated).then(|| {
        format!(
            "Nomad finds no node for the job: {filtered} of {evaluated} nodes do not fit its \
             datacenters, node pool, constraints or resources"
        )
    })
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

/// The exit code of the task, and a note for its standard error; a job that could not run the
/// task is an error that another attempt may not have.
fn outcome(
    recorded: Option<i32>,
    ended: &Ended,
    time: Option<u64>,
) -> Result<(i32, Option<String>), ExecutorError> {
    let message = &ended.message;
    match (ended.exit_code, recorded) {
        (_, Some(TIMED_OUT)) | (Some(TIMED_OUT), None) if time.is_some() => {
            let time = time.unwrap_or_default();
            let note = format!("uwe: the task ran longer than its time of {time} seconds\n");
            Ok((143, Some(note)))
        }
        // The wrapper records the code of the task first, which wins over Nomad's state.
        (_, Some(recorded)) => Ok((recorded, None)),
        (Some(status), _) if ended.memory && status != 0 => Ok((
            status,
            Some(format!(
                "uwe: Nomad stopped the task for lack of memory (exit code {status})\n"
            )),
        )),
        (status, _) => {
            let status = status.map_or("no exit code".to_owned(), |code| {
                format!("exit code {code}")
            });
            Err(machine(format!(
                "the Nomad job could not run the task ({status}): {message}"
            )))
        }
    }
}

pub fn machine(message: impl Into<String>) -> ExecutorError {
    ExecutorError::Engine {
        engine: "Nomad".into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn ended(failed: bool, exit_code: Option<i32>, memory: bool) -> Ended {
        Ended {
            failed,
            exit_code,
            alloc: "alloc-1".into(),
            message: "Terminated: Exit Code: 137".into(),
            memory,
        }
    }

    #[test]
    fn reads_allocations() {
        assert_eq!(seen(&[]), Seen::Unplaced);
        let running =
            json!({"ClientStatus": "running", "TaskStates": {"task": {"State": "running"}}});
        assert_eq!(seen(&[running]), Seen::Running);
        let failed = json!({"ID": "alloc-2", "ClientStatus": "failed", "TaskStates": {"task": {
        "State": "dead", "Failed": true, "Events": [
            {"Type": "Started", "ExitCode": 0},
            {"Type": "Terminated", "ExitCode": 137, "Details": {"oom_killed": "true"},
             "DisplayMessage": "Exit Code: 137, Exit Message: \"OOM Killed\""},
            {"Type": "Not Restarting", "ExitCode": 0, "DisplayMessage": "Policy allows no restarts"},
        ]}}});
        let Seen::Ended(end) = seen(&[failed]) else {
            panic!("an end");
        };
        assert_eq!(
            (end.failed, end.exit_code, end.memory),
            (true, Some(137), true)
        );
        assert_eq!(end.alloc, "alloc-2");
        assert!(
            end.message.contains("Policy allows no restarts"),
            "{}",
            end.message
        );
    }

    #[test]
    fn joins_job_outcomes() {
        assert_eq!(
            outcome(Some(0), &ended(false, Some(0), false), None).ok(),
            Some((0, None))
        );
        // A failed allocation whose wrapper recorded 0 keeps the wrapper's code.
        assert_eq!(
            outcome(Some(0), &ended(true, Some(1), false), None).ok(),
            Some((0, None))
        );
        assert_eq!(
            outcome(Some(3), &ended(true, Some(3), false), None).ok(),
            Some((3, None))
        );
        let timed = outcome(
            Some(TIMED_OUT),
            &ended(true, Some(TIMED_OUT), false),
            Some(60),
        );
        assert_eq!(timed.ok().map(|(code, _)| code), Some(143));
        let memory = outcome(None, &ended(true, Some(137), true), None).expect("an outcome");
        assert!(memory.1.is_some_and(|note| note.contains("lack of memory")));
        for code in [Some(125), None] {
            let lost = outcome(None, &ended(true, code, false), None);
            assert!(
                matches!(lost, Err(ExecutorError::Engine { .. })),
                "{lost:?}"
            );
        }
    }

    #[test]
    fn finds_unplaceable_jobs() {
        let filtered = json!([{"ModifyIndex": 1, "FailedTGAllocs": {"uwe": {
            "NodesEvaluated": 3, "NodesFiltered": 3, "ConstraintFiltered": {"${attr.cpu.arch} = arm64": 3}}}}]);
        let filtered = filtered.as_array().expect("a list");
        assert!(unplaceable(filtered).is_some_and(|text| text.contains("3 of 3")));
        let busy = json!([{"FailedTGAllocs": {"uwe": {"NodesEvaluated": 3, "NodesFiltered": 1,
                                                       "NodesExhausted": 2}}}]);
        assert_eq!(unplaceable(busy.as_array().expect("a list")), None);
        let placed = json!([{"Status": "complete"}]);
        assert_eq!(unplaceable(placed.as_array().expect("a list")), None);
    }
}
