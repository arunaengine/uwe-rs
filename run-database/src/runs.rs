//! Reads runs back, with the standard output and error of their tasks and their events.
// Copyright (c) 2026 The iunctus Contributors
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
}

/// The standard output or error of an attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Log {
    pub digest: String,
    pub size: u64,
    /// The whole text, or its end when `whole` is false.
    pub text: String,
    pub whole: bool,
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

const RUN_COLUMNS: &str = "id, lineage, resumes, workflow, language, revision, arguments,
    parameters, executor, simulated, store, process, started, ended, state, error";

impl Database {
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

    /// The standard output (`stdout`) or error (`stderr`) of an attempt, if it wrote any.
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
                "SELECT digest, size, text FROM logs
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

    /// Adds the number of tasks in each state, and marks a run whose process is gone as stopped.
    fn counted(&self, mut run: Run) -> Result<Run, DatabaseError> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT state, COUNT(*) FROM tasks WHERE run = ?1 GROUP BY state")?;
        run.tasks = statement
            .query_map([&run.id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
        if run.state == "running" && !alive(run.process) {
            run.state = "stopped".into();
        }
        Ok(run)
    }
}

/// Whether a process of this machine still runs. Only Linux can tell without a system call;
/// elsewhere every process counts as alive.
fn alive(process: u32) -> bool {
    let proc = std::path::Path::new("/proc");
    !proc.is_dir() || proc.join(process.to_string()).exists()
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
    };
    Ok((run, row.get(6)?, row.get(7)?))
}

/// A run with its command line and parameters read from their JSON.
fn parsed((mut run, arguments, parameters): (Run, String, String)) -> Result<Run, DatabaseError> {
    run.arguments = serde_json::from_str(&arguments)?;
    run.parameters = serde_json::from_str(&parameters)?;
    Ok(run)
}
