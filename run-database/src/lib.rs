//! The run database (decision 10): one SQLite file that records every run, simulated or real,
//! in the same form for all languages. It holds each run with its workflow, revision, command
//! line and parameters; each task that the driver asked for with its description; each attempt
//! with its times and exit code; the input and output files with their locations; the edges of
//! the execution graph; the standard output and error of each attempt; the events in the order
//! the engine saw them; the JSON Schemas of the inputs, with the values of the run, and of the
//! outputs; and what pins the run: the files of its workflow, the files its command line names
//! and the remote files it read, by digest, and the verdicts of the reproducibility check.
//!
//! A [`Recorder`] writes one run while it goes on. The web page, the RO-Crate export, resume and
//! the insight commands read the database through [`Database`]. SQLite runs in WAL mode, so a
//! page can read a run while another process records it. The SQL is kept portable, so that
//! another engine can replace SQLite behind this interface later (decisions, open questions).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod chains;
mod compare;
mod fingerprint;
mod graph;
mod keep;
mod origin;
mod pinned;
mod pins;
mod recorder;
mod resume;
mod runs;
mod schema;
mod tasks;
mod time;
mod why;

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

pub use compare::{Comparison, Pairing, Side, TaskPair};
pub use fingerprint::{InputState, fingerprint, input_states};
pub use keep::References;
pub use origin::Origin;
pub use pinned::{CheckRow, ImagePin, RemoteFile, Source, SourceKind};
pub use recorder::{LOG_LIMIT, Recorder, RunStart};
pub use resume::Reusable;
pub use runs::{Event, Log, Run, RunInputs};
pub use schema::VERSION;
pub use tasks::{Attempt, EdgeRow, FileRow, MarkerRow, TaskDetail, TaskRow};
pub use time::{PathTask, StepTime, TimeSpent};
pub use why::Why;

/// The name of the database file in the cache directory, next to the default store.
pub const FILE_NAME: &str = "runs.sqlite";

/// How long a connection waits for another process that holds the write lock.
const BUSY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum DatabaseError {
    #[error("run database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("run database: {0}")]
    Io(#[from] std::io::Error),
    #[error("run database: {0}")]
    Json(#[from] serde_json::Error),
    #[error(
        "the run database has schema version {found}, which a newer iunctus wrote; this one knows \
         versions up to {known}"
    )]
    Newer { found: u32, known: u32 },
    #[error("the run database has no run `{0}`")]
    UnknownRun(String),
}

/// A connection to the run database, for reading runs and for starting a [`Recorder`].
pub struct Database {
    connection: Connection,
}

impl Database {
    /// Opens the database file, creating it and its folder when missing, and brings its schema
    /// to the current [`VERSION`]. Runs that open a new file at the same moment take turns.
    pub fn open(path: &Path) -> Result<Self, DatabaseError> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let begun = std::time::Instant::now();
        loop {
            match Self::connect(path) {
                // SQLite gives up at once on some locks, like the one that turns a new file
                // into WAL mode while another connection sets it up too.
                Err(DatabaseError::Sqlite(error))
                    if busy(&error) && begun.elapsed() < BUSY_TIMEOUT =>
                {
                    std::thread::sleep(Duration::from_millis(20));
                }
                result => return result,
            }
        }
    }

    /// A connection to the file, set up and with the current schema.
    fn connect(path: &Path) -> Result<Self, DatabaseError> {
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        // WAL lets readers go on while a run writes; NORMAL syncs at checkpoints only, which
        // is safe in WAL mode: a crash can lose the last commits, never break the file.
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        schema::migrate(&mut connection)?;
        Ok(Database { connection })
    }

    /// The schema version of the file, which [`Database::open`] brought to [`VERSION`].
    pub fn version(&self) -> Result<u32, DatabaseError> {
        Ok(self
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))?)
    }

    /// Starts recording a run. The recorder owns the connection from now on.
    pub fn record(self, run: &RunStart) -> Result<Recorder, DatabaseError> {
        Recorder::start(self.connection, run)
    }
}

/// Whether SQLite could not get a lock that another connection holds.
fn busy(error: &rusqlite::Error) -> bool {
    use rusqlite::ErrorCode;
    matches!(
        error.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

/// Milliseconds since the Unix epoch, the unit of every time in the database.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}
