//! Records and reads workflow runs, task attempts, files, events and reproducibility data.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod chains;
mod compare;
mod deliveries;
mod fingerprint;
mod graph;
mod journal;
mod keep;
mod measured;
mod origin;
mod pinned;
mod pins;
mod public;
mod recorder;
mod resume;
mod runs;
mod schema;
mod steps;
mod tasks;
mod time;
mod why;

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

pub use compare::{Comparison, Pairing, Side, TaskPair};
pub use deliveries::Delivered;
pub use fingerprint::{InputState, fingerprint, input_states};
pub use journal::{Journal, journal};
pub use keep::References;
pub use origin::Origin;
pub use pinned::{CheckRow, ImagePin, RemoteFile, Source, SourceKind};
pub use public::{public_json, public_resource_id, redact_public_text};
pub use recorder::{LOG_LIMIT, Recorder, RunStart};
pub use resume::Reusable;
pub use runs::{Event, Log, Owner, Run, RunInputs};
pub use schema::VERSION;
pub use steps::{StepChange, StepDifference};
pub use tasks::{Attempt, EdgeRow, FileRow, MarkerRow, TaskDetail, TaskRow};
pub use time::{PathTask, StepTime, TimeSpent};
pub use why::Why;

/// The name of the database file in the cache directory, next to the default store.
pub const FILE_NAME: &str = "runs.sqlite";

#[derive(Debug, thiserror::Error)]
pub enum DatabaseError {
    #[error("run database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("run database: {0}")]
    Io(#[from] std::io::Error),
    #[error("run database: {0}")]
    Json(#[from] serde_json::Error),
    #[error(
        "the run database has schema version {found}, which a newer uwe wrote; this one knows \
         versions up to {known}"
    )]
    Newer { found: u32, known: u32 },
    #[error("the run database has no run `{0}`")]
    UnknownRun(String),
    #[error("run `{0}` was explicitly stopped; its recorder cannot write again")]
    Stopped(String),
    #[error("run `{0}` still has a live local owner")]
    LiveOwner(String),
}

/// A connection to the run database, for reading runs and for starting a [`Recorder`].
pub struct Database {
    connection: Connection,
}

impl Database {
    /// Syncs each committed transaction before acknowledging strict provenance writes.
    pub fn durable(self) -> Result<Self, DatabaseError> {
        self.connection.pragma_update(None, "synchronous", "FULL")?;
        Ok(self)
    }

    /// Reads related rows from one committed snapshot.
    pub fn snapshot<T>(
        &self,
        read: impl FnOnce(&Self) -> Result<T, DatabaseError>,
    ) -> Result<T, DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        let result = read(self)?;
        transaction.commit()?;
        Ok(result)
    }

    /// Opens the database file, creating it and its folder when missing, and brings its schema
    /// to the current [`VERSION`]. Runs that open a new file at the same moment take turns.
    pub fn open(path: &Path) -> Result<Self, DatabaseError> {
        Self::open_as(path, journal(path))
    }

    /// Reads an existing database without migrations or writable WAL shared memory.
    /// WAL-mode files without existing WAL and SHM sidecars cannot be read this way.
    pub fn read_only(path: &Path) -> Result<Self, DatabaseError> {
        use std::io::Read;

        let path = std::path::absolute(path)?;
        let mut header = [0; 20];
        std::fs::File::open(&path)?.read_exact(&mut header)?;
        // SQLite would create a missing WAL before discovering unavailable shared memory.
        if header[19] == 2 {
            for suffix in ["-wal", "-shm"] {
                let mut sidecar = path.as_os_str().to_os_string();
                sidecar.push(suffix);
                std::fs::metadata(sidecar)?;
            }
        }
        let encoded: String = path
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .map(|byte| format!("%{byte:02X}"))
            .collect();
        let connection = Connection::open_with_flags(
            format!("file:{encoded}?mode=ro&readonly_shm=1"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        )?;
        Ok(Self { connection })
    }

    /// Caps lock waits for observers whose shutdown must not inherit long shared-store waits.
    pub fn open_with_wait(path: &Path, wait: Duration) -> Result<Self, DatabaseError> {
        let mut journal = journal(path);
        journal.wait = journal.wait.min(wait);
        Self::open_as(path, journal)
    }

    /// Opens the database file with this journal.
    pub(crate) fn open_as(path: &Path, journal: Journal) -> Result<Self, DatabaseError> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let begun = std::time::Instant::now();
        loop {
            match Self::connect(path, journal) {
                // SQLite gives up at once on some locks, like the one that turns a new file
                // into WAL mode while another connection sets it up too.
                Err(DatabaseError::Sqlite(error))
                    if busy(&error) && begun.elapsed() < journal.wait =>
                {
                    std::thread::sleep(Duration::from_millis(20));
                }
                result => return result,
            }
        }
    }

    /// A connection to the file, set up and with the current schema.
    fn connect(path: &Path, journal: Journal) -> Result<Self, DatabaseError> {
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(journal.wait)?;
        // WAL lets readers go on while a run writes; NORMAL syncs at checkpoints only, which
        // is safe in WAL mode: a crash can lose the last commits, never break the file. On a
        // file system that machines share, the rollback journal (see `journal`).
        connection.pragma_update(None, "journal_mode", journal.mode)?;
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

#[cfg(test)]
mod tests {
    #[test]
    fn rejects_missing_readonly() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("missing/runs.sqlite");
        assert!(super::Database::read_only(&path).is_err());
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn reads_wal_pins() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("runs ?#%.sqlite");
        let writer = rusqlite::Connection::open(&path).unwrap();
        writer
            .execute_batch(
                "PRAGMA journal_mode = WAL; PRAGMA user_version = 1;
             CREATE TABLE runs (id TEXT, simulated INTEGER);
             CREATE TABLE tasks (run TEXT, id INTEGER, description TEXT);
             CREATE TABLE attempts (run TEXT, task INTEGER, image TEXT, started INTEGER);
             INSERT INTO runs VALUES ('run', 0);
             INSERT INTO tasks VALUES ('run', 1, '{\"image\":\"image:tag\"}');
             INSERT INTO attempts VALUES ('run', 1, 'image@sha256:abc', 1);",
            )
            .unwrap();
        let before: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                let bytes = std::fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect();
        assert_eq!(before.len(), 3);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::reads_readonly_child"])
            .env("UWE_READONLY_DATABASE", &path)
            .status()
            .unwrap();
        assert!(status.success());
        for (path, bytes) in &before {
            assert_eq!(&std::fs::read(path).unwrap(), bytes, "{}", path.display());
        }
        assert_eq!(
            std::fs::read_dir(root.path()).unwrap().count(),
            before.len()
        );
        drop(writer);
        let bytes = std::fs::read(&path).unwrap();
        assert!(super::Database::read_only(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn reads_readonly_child() {
        let Some(path) = std::env::var_os("UWE_READONLY_DATABASE") else {
            return;
        };
        let reader = super::Database::read_only(std::path::Path::new(&path)).unwrap();
        assert_eq!(reader.pins().unwrap()["image:tag"], "image@sha256:abc");
        assert_eq!(reader.version().unwrap(), 1);
        assert!(reader.connection.execute("DELETE FROM runs", []).is_err());
    }

    #[test]
    fn syncs_strict_commits() {
        let root = tempfile::tempdir().unwrap();
        let database = super::Database::open(&root.path().join("runs.sqlite"))
            .unwrap()
            .durable()
            .unwrap();
        let synchronous: i64 = database
            .connection
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();
        assert_eq!(synchronous, 2);
    }
}
