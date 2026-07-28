//! Reads the tasks of a run back: their states, attempts, files and the edges between them.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;
use workflow_graph::Task;

use crate::{CheckRow, Database, DatabaseError};

/// A task with its last attempt, as a row of a table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TaskRow {
    pub id: u64,
    pub step: String,
    pub name: String,
    pub state: String,
    pub requested: i64,
    /// The number of attempts so far.
    pub attempts: u32,
    pub started: Option<i64>,
    pub ended: Option<i64>,
    pub exit_code: Option<i32>,
    pub reused: Option<String>,
}

/// One attempt of a task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Attempt {
    pub number: u32,
    pub started: i64,
    pub ended: Option<i64>,
    pub exit_code: Option<i32>,
    pub reused: Option<String>,
    pub image: Option<String>,
    pub retried: bool,
    /// What the attempt used, when the executor measured it: CPU time in microseconds, the
    /// peak of memory and the bytes of block I/O.
    pub cpu_usec: Option<i64>,
    pub memory_peak: Option<i64>,
    pub read_bytes: Option<i64>,
    pub write_bytes: Option<i64>,
}

/// An input or output file of a task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FileRow {
    pub task: u64,
    /// The attempt that made an output; `None` for an input.
    pub attempt: Option<u32>,
    pub role: String,
    pub path: String,
    pub location: String,
    pub directory: bool,
    pub digest: Option<String>,
    pub size: Option<u64>,
}

/// An edge of the execution graph.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EdgeRow {
    pub source: u64,
    pub target: u64,
    pub kind: String,
    pub path: Option<String>,
}

/// A point where real data can change the tasks after a task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MarkerRow {
    pub task: u64,
    pub kind: String,
    pub reason: String,
}

/// Everything about one task.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TaskDetail {
    pub row: TaskRow,
    /// The task as the driver asked for it.
    pub description: Task,
    pub fingerprint: String,
    pub attempts: Vec<Attempt>,
    pub inputs: Vec<FileRow>,
    /// The outputs of the last attempt.
    pub outputs: Vec<FileRow>,
    /// The verdicts of the reproducibility check of its attempts.
    pub checks: Vec<CheckRow>,
}

const TASK_ROW: &str = "SELECT t.id, t.step, t.name, t.state, t.requested,
        (SELECT COUNT(*) FROM attempts WHERE run = t.run AND task = t.id),
        a.started, a.ended, a.exit_code, a.reused
    FROM tasks t LEFT JOIN attempts a ON a.run = t.run AND a.task = t.id
        AND a.number = (SELECT MAX(number) FROM attempts WHERE run = t.run AND task = t.id)";

impl Database {
    /// The tasks of a run with their last attempt, in the order they were asked for.
    pub fn tasks(&self, run: &str) -> Result<Vec<TaskRow>, DatabaseError> {
        let mut statement = self
            .connection
            .prepare(&format!("{TASK_ROW} WHERE t.run = ?1 ORDER BY t.rowid"))?;
        let rows = statement
            .query_map([run], task_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn task_row(&self, run: &str, task: u64) -> Result<Option<TaskRow>, DatabaseError> {
        let query = format!("{TASK_ROW} WHERE t.run = ?1 AND t.id = ?2");
        let row = self
            .connection
            .query_row(&query, params![run, task], task_row)
            .optional()?;
        Ok(row)
    }

    /// A task with its description, attempts, inputs and the outputs of its last attempt.
    pub fn task(&self, run: &str, task: u64) -> Result<Option<TaskDetail>, DatabaseError> {
        let Some(row) = self.task_row(run, task)? else {
            return Ok(None);
        };
        let (description, fingerprint): (String, String) = self.connection.query_row(
            "SELECT description, fingerprint FROM tasks WHERE run = ?1 AND id = ?2",
            params![run, task],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let mut statement = self.connection.prepare(
            "SELECT number, started, ended, exit_code, reused, image, retried, cpu_usec,
                    memory_peak, read_bytes, write_bytes FROM attempts
             WHERE run = ?1 AND task = ?2 ORDER BY number",
        )?;
        let attempts = statement
            .query_map(params![run, task], |row| {
                Ok(Attempt {
                    number: row.get(0)?,
                    started: row.get(1)?,
                    ended: row.get(2)?,
                    exit_code: row.get(3)?,
                    reused: row.get(4)?,
                    image: row.get(5)?,
                    retried: row.get(6)?,
                    cpu_usec: row.get(7)?,
                    memory_peak: row.get(8)?,
                    read_bytes: row.get(9)?,
                    write_bytes: row.get(10)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let files = self.files(run, Some(task))?;
        let last = attempts.last().map(|attempt| attempt.number);
        let (inputs, outputs) = files
            .into_iter()
            .partition::<Vec<_>, _>(|file| file.attempt.is_none());
        let outputs = outputs
            .into_iter()
            .filter(|file| file.attempt == last)
            .collect();
        Ok(Some(TaskDetail {
            row,
            description: serde_json::from_str(&description)?,
            fingerprint,
            attempts,
            inputs,
            outputs,
            checks: self.checks(run, Some(task))?,
        }))
    }

    /// The files of a run, or of one of its tasks, in the order they were recorded.
    pub fn files(&self, run: &str, task: Option<u64>) -> Result<Vec<FileRow>, DatabaseError> {
        let mut statement = self.connection.prepare(
            "SELECT task, attempt, role, path, location, directory, digest, size FROM files
             WHERE run = ?1 AND (?2 IS NULL OR task = ?2) ORDER BY rowid",
        )?;
        let files = statement
            .query_map(params![run, task], file_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(files)
    }

    /// The outputs of every run that are at a location, the latest first: where a file came from.
    pub fn producers(&self, location: &str) -> Result<Vec<(String, FileRow)>, DatabaseError> {
        let mut statement = self.connection.prepare(
            "SELECT f.task, f.attempt, f.role, f.path, f.location, f.directory, f.digest, f.size,
                f.run
             FROM files f JOIN runs r ON r.id = f.run
             WHERE f.location = ?1 AND f.role = 'output' ORDER BY r.started DESC",
        )?;
        let files = statement
            .query_map([location], |row| Ok((row.get(8)?, file_row(row)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(files)
    }

    /// The digest of the output of any run at a location, when one run made it and its
    /// executor knew the digest.
    pub fn digest_of(&self, location: &str) -> Result<Option<String>, DatabaseError> {
        let digest = self
            .connection
            .prepare_cached(
                "SELECT digest FROM files
                 WHERE location = ?1 AND role = 'output' AND digest IS NOT NULL LIMIT 1",
            )?
            .query_row([location], |row| row.get(0))
            .optional()?;
        Ok(digest)
    }

    /// The markers of a run, in the order they were found.
    pub fn markers(&self, run: &str) -> Result<Vec<MarkerRow>, DatabaseError> {
        let mut statement = self
            .connection
            .prepare("SELECT task, kind, reason FROM markers WHERE run = ?1 ORDER BY rowid")?;
        let markers = statement
            .query_map([run], |row| {
                Ok(MarkerRow {
                    task: row.get(0)?,
                    kind: row.get(1)?,
                    reason: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(markers)
    }

    pub fn edges(&self, run: &str) -> Result<Vec<EdgeRow>, DatabaseError> {
        let mut statement = self.connection.prepare(
            "SELECT source, target, kind, path FROM edges WHERE run = ?1 ORDER BY rowid",
        )?;
        let edges = statement
            .query_map([run], |row| {
                Ok(EdgeRow {
                    source: row.get(0)?,
                    target: row.get(1)?,
                    kind: row.get(2)?,
                    path: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(edges)
    }
}

fn task_row(row: &Row) -> rusqlite::Result<TaskRow> {
    Ok(TaskRow {
        id: row.get(0)?,
        step: row.get(1)?,
        name: row.get(2)?,
        state: row.get(3)?,
        requested: row.get(4)?,
        attempts: row.get(5)?,
        started: row.get(6)?,
        ended: row.get(7)?,
        exit_code: row.get(8)?,
        reused: row.get(9)?,
    })
}

fn file_row(row: &Row) -> rusqlite::Result<FileRow> {
    Ok(FileRow {
        task: row.get(0)?,
        attempt: row.get(1)?,
        role: row.get(2)?,
        path: row.get(3)?,
        location: row.get(4)?,
        directory: row.get(5)?,
        digest: row.get(6)?,
        size: row.get(7)?,
    })
}
