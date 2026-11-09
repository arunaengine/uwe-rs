//! Reads runs back, with the standard output and error of their tasks and their events.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;

use crate::{Database, DatabaseError};

/// A run, with the number of its tasks in each state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Run {
    pub id: String,
    pub lineage: String,
    pub resumes: Option<String>,
    pub workflow: String,
    pub language: String,
    pub revision: Option<String>,
    pub arguments: Vec<String>,
    pub parameters: Vec<(String, String)>,
    pub executor: String,
    pub simulated: bool,
    pub store: Option<String>,
    pub process: u32,
    pub started: i64,
    pub ended: Option<i64>,
    /// `running`, `succeeded` or `failed`, or `stopped` for a run whose process ended before
    /// it could say how the run ended.
    pub state: String,
    pub error: Option<String>,
    /// The number of tasks in each state, like `succeeded: 12`.
    pub tasks: BTreeMap<String, u64>,
    /// The uwe that ran it, like `uwe 0.1.0`; unknown for runs of older versions.
    pub program: Option<String>,
    /// The folder the run started in, where relative paths of its command line start.
    pub launch: Option<String>,
    /// The folder of the workflow whose files the run recorded as its sources.
    pub workflow_folder: Option<String>,
    /// The run that this one repeats.
    pub repeats: Option<String>,
    /// For a replay of one task, the run and the task that it replays.
    pub replays: Option<String>,
    pub replays_task: Option<u64>,
    /// The wir document that the run compiled its workflow into, and ran.
    pub document: Option<String>,
    /// The digest of the document, like `sha256:<hex>`.
    pub document_digest: Option<String>,
}

/// The inputs of a run (decision 24): the JSON Schema of the inputs of its workflow, and the
/// values of the run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RunInputs {
    pub schema: serde_json::Value,
    pub values: serde_json::Value,
}

/// The standard output or error of an attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Log {
    pub digest: String,
    pub size: u64,
    /// The whole text, or its end when `whole` is false.
    pub text: String,
    pub whole: bool,
    /// Retained bytes for a deferred or truncated log; absent in historical records.
    pub location: Option<String>,
}

/// One event of a run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Event {
    pub sequence: i64,
    pub time: i64,
    pub kind: String,
    pub task: Option<u64>,
    pub attempt: Option<u32>,
}

/// An observed owner and event cursor used to fence an explicit stopped-owner reconciliation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owner {
    pub run: String,
    pub process: u32,
    pub host: Option<String>,
    pub boot: Option<String>,
    pub start: Option<String>,
    pub sequence: i64,
}

const RUN_COLUMNS: &str = "id, lineage, resumes, workflow, language, revision, arguments,
    parameters, executor, simulated, store, process, started, ended, state, error, program,
    launch, workflow_folder, repeats, replays, replays_task, document, document_digest";

impl Database {
    /// Captures the owner of a running record without guessing whether a foreign host is alive.
    pub fn owner(&self, run: &str) -> Result<Option<Owner>, DatabaseError> {
        Ok(self
            .connection
            .query_row(
                "SELECT process, process_host, process_boot, process_start,
                (SELECT COALESCE(MAX(sequence), 0) FROM events WHERE run = ?1)
             FROM runs WHERE id = ?1 AND state = 'running'",
                [run],
                |row| {
                    Ok(Owner {
                        run: run.to_owned(),
                        process: row.get(0)?,
                        host: row.get(1)?,
                        boot: row.get(2)?,
                        start: row.get(3)?,
                        sequence: row.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    /// The caller must independently confirm the exact foreign owner has stopped.
    /// A changed owner/cursor is refused, and a provably live local owner is always protected.
    pub fn reconcile(&mut self, owner: &Owner, reason: &str) -> Result<bool, DatabaseError> {
        if let Some((host, boot, start)) = process_identity(owner.process)
            && owner.host.as_deref() == Some(host.as_str())
            && owner.boot.as_deref() == Some(boot.as_str())
            && owner.start.as_deref() == Some(start.as_str())
        {
            return Err(DatabaseError::LiveOwner(owner.run.clone()));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let time = crate::now();
        let changed = transaction.execute(
            "UPDATE runs SET state = 'stopped', ended = ?7, error = ?8
             WHERE id = ?1 AND state = 'running' AND process = ?2
                AND process_host IS ?3 AND process_boot IS ?4 AND process_start IS ?5
                AND (SELECT COALESCE(MAX(sequence), 0) FROM events WHERE run = ?1) = ?6",
            params![
                owner.run,
                owner.process,
                owner.host,
                owner.boot,
                owner.start,
                owner.sequence,
                time,
                reason
            ],
        )?;
        if changed == 0 {
            return Ok(false);
        }
        transaction.execute(
            "UPDATE tasks SET state = 'stopped' WHERE run = ?1 AND state IN ('waiting', 'running')",
            [&owner.run],
        )?;
        transaction.execute(
            "INSERT INTO events (run, sequence, time, kind) VALUES (?1, ?2, ?3, 'ended')",
            params![owner.run, owner.sequence + 1, time],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    /// The latest runs first, at most `limit` of them.
    pub fn runs(&self, limit: u32) -> Result<Vec<Run>, DatabaseError> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM runs ORDER BY started DESC, rowid DESC LIMIT ?1"
        ))?;
        let runs = statement
            .query_map([limit], run_row)?
            .collect::<Result<Vec<_>, _>>()?;
        runs.into_iter()
            .map(|run| self.counted(parsed(run)?))
            .collect()
    }

    pub fn run(&self, id: &str) -> Result<Option<Run>, DatabaseError> {
        let run = self
            .connection
            .query_row(
                &format!("SELECT {RUN_COLUMNS} FROM runs WHERE id = ?1"),
                [id],
                run_row,
            )
            .optional()?;
        run.map(|run| self.counted(parsed(run)?)).transpose()
    }

    /// The runs that replay a task of a run, the latest first.
    pub fn replays_of(&self, run: &str, task: u64) -> Result<Vec<Run>, DatabaseError> {
        let mut statement = self.connection.prepare(
            "SELECT id FROM runs WHERE replays = ?1 AND replays_task = ?2
             ORDER BY started DESC, rowid DESC",
        )?;
        let ids = statement
            .query_map(params![run, task], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut runs = Vec::with_capacity(ids.len());
        for id in ids {
            runs.extend(self.run(&id)?);
        }
        Ok(runs)
    }

    /// The inputs of a run, when its driver described them.
    pub fn run_inputs(&self, run: &str) -> Result<Option<RunInputs>, DatabaseError> {
        let row: Option<(Option<String>, Option<String>)> = self
            .connection
            .query_row(
                "SELECT input_schema, inputs FROM runs WHERE id = ?1",
                [run],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((Some(schema), values)) = row else {
            return Ok(None);
        };
        Ok(Some(RunInputs {
            schema: serde_json::from_str(&schema)?,
            values: match values {
                Some(values) => serde_json::from_str(&values)?,
                None => serde_json::Value::Null,
            },
        }))
    }

    /// The standard output (`stdout`) or error (`stderr`) of an attempt, if it wrote any. The
    /// other containers of a task that runs several have theirs as `<container>/stdout` and
    /// `<container>/stderr`.
    pub fn log(
        &self,
        run: &str,
        task: u64,
        attempt: u32,
        stream: &str,
    ) -> Result<Option<Log>, DatabaseError> {
        let log = self
            .connection
            .query_row(
                "SELECT digest, size, text, location FROM logs
                 WHERE run = ?1 AND task = ?2 AND attempt = ?3 AND stream = ?4",
                params![run, task, attempt, stream],
                |row| {
                    let size: u64 = row.get(1)?;
                    let text: String = row.get(2)?;
                    Ok(Log {
                        digest: row.get(0)?,
                        whole: text.len() as u64 == size,
                        size,
                        text,
                        location: row.get(3)?,
                    })
                },
            )
            .optional()?;
        Ok(log)
    }

    /// The number of the last event of a run; 0 before the first.
    pub fn last_event(&self, run: &str) -> Result<i64, DatabaseError> {
        let sequence = self.connection.query_row(
            "SELECT COALESCE(MAX(sequence), 0) FROM events WHERE run = ?1",
            [run],
            |row| row.get(0),
        )?;
        Ok(sequence)
    }

    /// The events of a run after the one numbered `after`, at most `limit` of them.
    pub fn events(&self, run: &str, after: i64, limit: u32) -> Result<Vec<Event>, DatabaseError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT sequence, time, kind, task, attempt FROM events
             WHERE run = ?1 AND sequence > ?2 ORDER BY sequence LIMIT ?3",
        )?;
        let events = statement
            .query_map(params![run, after, limit], |row| {
                Ok(Event {
                    sequence: row.get(0)?,
                    time: row.get(1)?,
                    kind: row.get(2)?,
                    task: row.get(3)?,
                    attempt: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(events)
    }

    /// The time of the latest event of a run, if it has events.
    pub fn last_event_time(&self, run: &str) -> Result<Option<i64>, DatabaseError> {
        Ok(self.connection.query_row(
            "SELECT MAX(time) FROM events WHERE run = ?1",
            [run],
            |row| row.get(0),
        )?)
    }

    /// Events of one task, or the root run span when `task` is absent.
    pub fn span_events(&self, run: &str, task: Option<u64>) -> Result<Vec<Event>, DatabaseError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT sequence, time, kind, task, attempt FROM events
             WHERE run = ?1 AND task IS ?2 ORDER BY sequence",
        )?;
        Ok(statement
            .query_map(params![run, task], |row| {
                Ok(Event {
                    sequence: row.get(0)?,
                    time: row.get(1)?,
                    kind: row.get(2)?,
                    task: row.get(3)?,
                    attempt: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Adds the number of tasks in each state, and marks a run whose process is gone as stopped.
    fn counted(&self, mut run: Run) -> Result<Run, DatabaseError> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT state, COUNT(*) FROM tasks WHERE run = ?1 GROUP BY state")?;
        run.tasks = statement
            .query_map([&run.id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
        if run.state == "running" {
            let identity = self.connection.query_row(
                "SELECT process_host, process_boot, process_start FROM runs WHERE id = ?1",
                [&run.id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )?;
            if let (Some(host), Some(boot), Some(start)) = identity
                && !alive(run.process, &host, &boot, &start)
            {
                run.state = "stopped".into();
            }
        }
        Ok(run)
    }
}

pub(crate) fn process_identity(process: u32) -> Option<(String, String, String)> {
    let (host, boot) = machine_identity()?;
    Some((host, boot, process_start(process).ok()??))
}

fn machine_identity() -> Option<(String, String)> {
    let host = std::fs::read_to_string("/etc/machine-id")
        .or_else(|_| std::fs::read_to_string("/var/lib/dbus/machine-id"))
        .ok()?;
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let namespace = std::fs::read_link("/proc/self/ns/pid").ok()?;
    if host.trim().is_empty() || boot.trim().is_empty() {
        return None;
    }
    Some((
        format!("{}:{}", host.trim(), namespace.to_str()?),
        boot.trim().to_owned(),
    ))
}

fn process_start(process: u32) -> std::io::Result<Option<String>> {
    let stat = match std::fs::read_to_string(format!("/proc/{process}/stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let start = stat_start(&stat).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid process stat")
    })?;
    Ok(start.map(str::to_owned))
}

fn stat_start(stat: &str) -> Option<Option<&str>> {
    let (_, fields) = stat.rsplit_once(')')?;
    let mut fields = fields.split_whitespace();
    if matches!(fields.next()?, "Z" | "X") {
        return Some(None);
    }
    let start = fields.nth(18)?;
    start.parse::<u64>().ok()?;
    Some(Some(start))
}

fn alive(process: u32, host: &str, boot: &str, start: &str) -> bool {
    let Some((here, current_boot)) = machine_identity() else {
        return true;
    };
    if host != here || boot.is_empty() || start.parse::<u64>().is_err() {
        return true;
    }
    if boot != current_boot {
        return false;
    }
    match process_start(process) {
        Ok(Some(current)) => current == start,
        Ok(None) => false,
        Err(_) => true,
    }
}

/// A run from a row of [`RUN_COLUMNS`], with its command line and parameters still as JSON.
fn run_row(row: &Row) -> rusqlite::Result<(Run, String, String)> {
    let run = Run {
        id: row.get(0)?,
        lineage: row.get(1)?,
        resumes: row.get(2)?,
        workflow: row.get(3)?,
        language: row.get(4)?,
        revision: row.get(5)?,
        arguments: Vec::new(),
        parameters: Vec::new(),
        executor: row.get(8)?,
        simulated: row.get(9)?,
        store: row.get(10)?,
        process: row.get(11)?,
        started: row.get(12)?,
        ended: row.get(13)?,
        state: row.get(14)?,
        error: row.get(15)?,
        tasks: BTreeMap::new(),
        program: row.get(16)?,
        launch: row.get(17)?,
        workflow_folder: row.get(18)?,
        repeats: row.get(19)?,
        replays: row.get(20)?,
        replays_task: row.get(21)?,
        document: row.get(22)?,
        document_digest: row.get(23)?,
    };
    Ok((run, row.get(6)?, row.get(7)?))
}

/// A run with its command line and parameters read from their JSON.
fn parsed((mut run, arguments, parameters): (Run, String, String)) -> Result<Run, DatabaseError> {
    run.arguments = serde_json::from_str(&arguments)?;
    run.parameters = serde_json::from_str(&parameters)?;
    Ok(run)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_process_state() {
        let fields = std::iter::once("S")
            .chain(std::iter::repeat_n("0", 18))
            .chain(std::iter::once("1234"))
            .collect::<Vec<_>>()
            .join(" ");
        let stat = format!("42 (name with ) parentheses) {fields}");
        assert_eq!(stat_start(&stat), Some(Some("1234")));
        assert_eq!(stat_start(&stat.replace(") S ", ") Z ")), Some(None));
        assert_eq!(stat_start("malformed"), None);
    }
}
