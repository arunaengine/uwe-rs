//! Writes one run into the database while it goes on, one call per event of the engine.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use rusqlite::{Connection, OptionalExtension, params};
use task_model::Digest;
use workflow_graph::{Edge, EdgeKind, Event, Language, Observer, Outcome, Task, TaskId};

use crate::fingerprint::{fingerprint, input_states};
use crate::{DatabaseError, now};

/// The longest text of a standard output or error that the database keeps, in bytes. A longer
/// text keeps its end, where errors are; a real run has the whole text in its store.
pub const LOG_LIMIT: usize = 64 * 1024;

/// How long writes gather in one transaction before they are committed. Readers, like the web
/// page, see a run at most this much later; a run of many short tasks commits far less often.
/// A thread commits an open transaction after this time even when no write follows, so that a
/// long task never holds the write lock that other iunctus processes wait for.
const BATCH: Duration = Duration::from_millis(250);

/// What is known of a run when it starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunStart {
    pub id: String,
    /// The run this one resumes; the new run joins its lineage.
    pub resumes: Option<String>,
    /// The workflow file or directory, as an absolute path.
    pub workflow: String,
    pub language: Language,
    /// The Git commit of the workflow's repository.
    pub revision: Option<String>,
    /// The command line after `iunctus`.
    pub arguments: Vec<String>,
    /// The workflow parameters, in the order given.
    pub parameters: Vec<(String, String)>,
    pub executor: String,
    /// Whether the tasks only pretend to run.
    pub simulated: bool,
    /// Where a real run keeps its files.
    pub store: Option<String>,
}

/// Records one run. A failed write does not stop the run: the recorder keeps the first error,
/// skips later writes, and [`Recorder::close`] returns the error.
pub struct Recorder {
    writer: Arc<Mutex<Writer>>,
    run: String,
    /// Stops the thread that commits, and waits for it.
    flusher: Option<(mpsc::Sender<()>, JoinHandle<()>)>,
}

/// The connection and the open transaction, shared with the thread that commits.
struct Writer {
    connection: Connection,
    /// The number of the last event.
    sequence: i64,
    /// When the open transaction began; `None` when no transaction is open.
    batch: Option<Instant>,
    error: Option<DatabaseError>,
}

impl Recorder {
    pub(crate) fn start(connection: Connection, run: &RunStart) -> Result<Self, DatabaseError> {
        let lineage = match &run.resumes {
            Some(resumed) => connection
                .query_row("SELECT lineage FROM runs WHERE id = ?1", [resumed], |row| {
                    row.get::<_, String>(0)
                })
                .optional()?
                .ok_or_else(|| DatabaseError::UnknownRun(resumed.clone()))?,
            None => run.id.clone(),
        };
        connection.execute(
            "INSERT INTO runs (id, lineage, resumes, workflow, language, revision, arguments,
                parameters, executor, simulated, store, process, started, state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 'running')",
            params![
                run.id,
                lineage,
                run.resumes,
                run.workflow,
                run.language.to_string(),
                run.revision,
                serde_json::to_string(&run.arguments)?,
                serde_json::to_string(&run.parameters)?,
                run.executor,
                run.simulated,
                run.store,
                std::process::id(),
                now(),
            ],
        )?;
        let writer = Arc::new(Mutex::new(Writer {
            connection,
            sequence: 0,
            batch: None,
            error: None,
        }));
        let (stop, stopped) = mpsc::channel::<()>();
        let shared = Arc::clone(&writer);
        let thread = std::thread::Builder::new()
            .name("iunctus-recorder".into())
            .spawn(move || {
                while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(BATCH) {
                    lock(&shared).commit_if_old();
                }
            })?;
        Ok(Recorder {
            writer,
            run: run.id.clone(),
            flusher: Some((stop, thread)),
        })
    }

    /// The id of the run.
    pub fn run(&self) -> &str {
        &self.run
    }

    /// The driver asked for a task; `edges` join it to the earlier tasks it waits for.
    pub fn requested(&mut self, task: &Task, edges: &[Edge]) {
        self.write(|connection, run, sequence| {
            let states = input_states(task);
            let time = now();
            connection
                .prepare_cached(
                    "INSERT INTO tasks (run, id, step, name, description, fingerprint, requested,
                        state)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'waiting')",
                )?
                .execute(params![
                    run,
                    task.id.0,
                    task.step,
                    task.name,
                    serde_json::to_string(task).map_err(to_sqlite)?,
                    fingerprint(task, &states),
                    time,
                ])?;
            let mut insert = connection.prepare_cached(
                "INSERT INTO files (run, task, attempt, role, path, location, directory, digest,
                    size)
                 VALUES (?1, ?2, NULL, 'input', ?3, ?4, ?5, ?6, ?7)",
            )?;
            for (input, state) in task.inputs.iter().zip(&states) {
                let (location, digest, size) = match &input.content {
                    Some(content) => (
                        "",
                        Some(Digest::sha256(content.as_bytes()).to_string()),
                        Some(content.len() as u64),
                    ),
                    None => (
                        input.source.as_str(),
                        None,
                        state.filter(|_| !input.directory).map(|state| state.size),
                    ),
                };
                insert.execute(params![
                    run,
                    task.id.0,
                    input.path,
                    location,
                    input.directory,
                    digest,
                    size
                ])?;
            }
            let mut insert = connection.prepare_cached(
                "INSERT INTO edges (run, source, target, kind, path) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for edge in edges {
                let (kind, path) = match &edge.kind {
                    EdgeKind::File { path } => ("file", Some(path.as_str())),
                    EdgeKind::Order => ("order", None),
                };
                insert.execute(params![run, edge.from.0, edge.to.0, kind, path])?;
            }
            event(connection, run, sequence, "requested", Some(task.id), None)
        });
    }

    /// The executor got an attempt of a task.
    pub fn started(&mut self, task: TaskId, attempt: u32) {
        self.write(|connection, run, sequence| {
            connection
                .prepare_cached(
                    "INSERT INTO attempts (run, task, number, started) VALUES (?1, ?2, ?3, ?4)",
                )?
                .execute(params![run, task.0, attempt, now()])?;
            connection
                .prepare_cached("UPDATE tasks SET state = 'running' WHERE run = ?1 AND id = ?2")?
                .execute(params![run, task.0])?;
            event(
                connection,
                run,
                sequence,
                "started",
                Some(task),
                Some(attempt),
            )
        });
    }

    /// An attempt of a task ended. With `retried`, the engine runs the task again, so the task
    /// is still running.
    pub fn finished(&mut self, outcome: &Outcome, attempt: u32, retried: bool) {
        self.write(|connection, run, sequence| {
            let task = outcome.id.0;
            // The executor knows the times best; a result from the cache ran nowhere.
            let time = |at: Option<SystemTime>| at.map(milliseconds);
            let reused = outcome.cached.then_some("cache");
            connection
                .prepare_cached(
                    "UPDATE attempts SET ended = ?4, exit_code = ?5, retried = ?6, image = ?7,
                        reused = COALESCE(?8, reused), started = COALESCE(?9, started)
                     WHERE run = ?1 AND task = ?2 AND number = ?3",
                )?
                .execute(params![
                    run,
                    task,
                    attempt,
                    time(outcome.ended).unwrap_or_else(now),
                    outcome.exit_code,
                    retried,
                    outcome.image,
                    reused,
                    time(outcome.started),
                ])?;
            let state = match (retried, outcome.exit_code) {
                (true, _) => "running",
                (false, 0) => "succeeded",
                (false, _) => "failed",
            };
            connection
                .prepare_cached("UPDATE tasks SET state = ?3 WHERE run = ?1 AND id = ?2")?
                .execute(params![run, task, state])?;
            let mut insert = connection.prepare_cached(
                "INSERT INTO files (run, task, attempt, role, path, location, directory, size)
                 VALUES (?1, ?2, ?3, 'output', ?4, ?5, ?6, ?7)",
            )?;
            for file in &outcome.files {
                let size = std::fs::metadata(&file.location)
                    .ok()
                    .filter(|metadata| metadata.is_file())
                    .map(|metadata| metadata.len());
                insert.execute(params![
                    run,
                    task,
                    attempt,
                    file.path,
                    file.location,
                    file.directory,
                    size
                ])?;
            }
            for (stream, text) in [("stdout", &outcome.stdout), ("stderr", &outcome.stderr)] {
                if text.is_empty() {
                    continue;
                }
                connection
                    .prepare_cached(
                        "INSERT INTO logs (run, task, attempt, stream, digest, size, text)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    )?
                    .execute(params![
                        run,
                        task,
                        attempt,
                        stream,
                        Digest::sha256(text.as_bytes()).to_string(),
                        text.len(),
                        tail(text)
                    ])?;
            }
            event(
                connection,
                run,
                sequence,
                "finished",
                Some(outcome.id),
                Some(attempt),
            )
        });
    }

    /// The result of an attempt came from elsewhere, not from running it: `source` is `cache`
    /// for the store, or the id of the earlier run that a resumed run took it from.
    pub fn reused(&mut self, task: TaskId, attempt: u32, source: &str) {
        self.write(|connection, run, sequence| {
            connection
                .prepare_cached(
                    "UPDATE attempts SET reused = ?4 WHERE run = ?1 AND task = ?2 AND number = ?3",
                )?
                .execute(params![run, task.0, attempt, source])?;
            event(
                connection,
                run,
                sequence,
                "reused",
                Some(task),
                Some(attempt),
            )
        });
    }

    /// The driver found that the tasks after a task depend on its outputs, so that a run with
    /// real data can have more or fewer tasks there. `kind` is `content`, `wildcard` or
    /// `checkpoint`.
    pub fn marked(&mut self, task: TaskId, kind: &str, reason: &str) {
        self.write(|connection, run, sequence| {
            connection
                .prepare_cached(
                    "INSERT INTO markers (run, task, kind, reason) VALUES (?1, ?2, ?3, ?4)",
                )?
                .execute(params![run, task.0, kind, reason])?;
            event(connection, run, sequence, "marked", Some(task), None)
        });
    }

    /// The run ended, with the error that stopped it if one did. Tasks that did not finish are
    /// marked as stopped, and everything is committed.
    pub fn ended(&mut self, error: Option<&str>) {
        self.write(|connection, run, sequence| {
            let state = if error.is_some() {
                "failed"
            } else {
                "succeeded"
            };
            connection.execute(
                "UPDATE runs SET ended = ?2, state = ?3, error = ?4 WHERE id = ?1",
                params![run, now(), state, error],
            )?;
            connection.execute(
                "UPDATE tasks SET state = 'stopped'
                 WHERE run = ?1 AND state IN ('waiting', 'running')",
                [run],
            )?;
            event(connection, run, sequence, "ended", None, None)
        });
        self.commit();
    }

    /// Commits what is left and returns the first error of any write.
    pub fn close(mut self) -> Result<(), DatabaseError> {
        self.stop();
        match lock(&self.writer).error.take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Runs one write, with the connection, the run id and the number of its event.
    fn write(&mut self, action: impl FnOnce(&Connection, &str, i64) -> rusqlite::Result<()>) {
        lock(&self.writer).write(&self.run, action);
    }

    fn commit(&mut self) {
        lock(&self.writer).commit();
    }

    /// Stops the thread that commits and commits what is left.
    fn stop(&mut self) {
        if let Some((stop, thread)) = self.flusher.take() {
            drop(stop);
            let _ = thread.join();
        }
        self.commit();
    }
}

impl Writer {
    /// Runs one write inside the open transaction, opening one when needed, and commits when
    /// the transaction is older than [`BATCH`].
    fn write(
        &mut self,
        run: &str,
        action: impl FnOnce(&Connection, &str, i64) -> rusqlite::Result<()>,
    ) {
        if self.error.is_some() {
            return;
        }
        if self.batch.is_none() {
            if let Err(error) = self.connection.execute_batch("BEGIN") {
                self.error = Some(error.into());
                return;
            }
            self.batch = Some(Instant::now());
        }
        self.sequence += 1;
        if let Err(error) = action(&self.connection, run, self.sequence) {
            self.error = Some(error.into());
            return;
        }
        self.commit_if_old();
    }

    fn commit_if_old(&mut self) {
        if self.batch.is_some_and(|began| began.elapsed() >= BATCH) {
            self.commit();
        }
    }

    fn commit(&mut self) {
        if self.batch.take().is_some()
            && let Err(error) = self.connection.execute_batch("COMMIT")
            && self.error.is_none()
        {
            self.error = Some(error.into());
        }
    }
}

/// The writer, also after a thread panicked while it held it: the writes before are whole.
fn lock(writer: &Mutex<Writer>) -> std::sync::MutexGuard<'_, Writer> {
    writer.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The engine tells the recorder each step of the run.
impl Observer for Recorder {
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Requested { task, edges } => self.requested(task, edges),
            Event::Started { task, attempt } => self.started(task, attempt),
            Event::Finished {
                outcome,
                attempt,
                retried,
            } => self.finished(outcome, attempt, retried),
            Event::Marked { task, marker } => self.marked(task, marker.kind.name(), &marker.reason),
            Event::Ended { error } => self.ended(error.map(ToString::to_string).as_deref()),
        }
    }
}

fn event(
    connection: &Connection,
    run: &str,
    sequence: i64,
    kind: &str,
    task: Option<TaskId>,
    attempt: Option<u32>,
) -> rusqlite::Result<()> {
    connection
        .prepare_cached(
            "INSERT INTO events (run, sequence, time, kind, task, attempt)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?
        .execute(params![
            run,
            sequence,
            now(),
            kind,
            task.map(|id| id.0),
            attempt
        ])?;
    Ok(())
}

/// A time as milliseconds since the Unix epoch.
fn milliseconds(time: SystemTime) -> i64 {
    time.duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

/// The end of a text, at most [`LOG_LIMIT`] bytes, cut at a character boundary.
fn tail(text: &str) -> &str {
    let mut start = text.len().saturating_sub(LOG_LIMIT);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

fn to_sqlite(error: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_texts_keep_their_end() {
        assert_eq!(tail("short"), "short");
        let long = format!("{}é{}", "a".repeat(LOG_LIMIT), "end");
        let kept = tail(&long);
        assert!(
            kept.len() <= LOG_LIMIT && kept.ends_with("éend"),
            "{}",
            kept.len()
        );
    }
}
