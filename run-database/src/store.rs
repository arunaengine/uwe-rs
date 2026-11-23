//! Lets a host keep runs elsewhere than in SQLite: the traits of a run store and two SQLite stores.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde_json::Value;
use workflow_graph::{Edge, ExecutionGraph, Outcome, Task, TaskId};

use crate::{
    Attempt, Comparison, Database, DatabaseError, Delivered, EdgeRow, Event, FileRow, ImagePin,
    Log, MarkerRow, Owner, RemoteFile, Reusable, Run, RunInputs, RunStart, Source, TaskDetail,
    TaskRow, TimeSpent, Why,
};

/// One change that a [`crate::Recorder`] makes to its run, as a [`RunStore`] receives it.
#[derive(Clone, Copy, Debug)]
pub enum Change<'a> {
    /// The driver asked for a task; the edges join it to the tasks it waits for.
    Requested {
        task: &'a Task,
        edges: &'a [Edge],
    },
    Started {
        task: TaskId,
        attempt: u32,
    },
    /// An attempt ended; with `retried` the engine runs the task again.
    Finished {
        outcome: &'a Outcome,
        attempt: u32,
        retried: bool,
    },
    Validated {
        task: TaskId,
        attempt: u32,
        accepted: bool,
    },
    /// The result came from `source`: `cache`, or the earlier run that a resumed run took.
    Reused {
        task: TaskId,
        attempt: u32,
        source: &'a str,
    },
    Marked {
        task: TaskId,
        kind: &'a str,
        reason: &'a str,
    },
    Inputs {
        schema: &'a Value,
        values: &'a Value,
    },
    Document {
        path: &'a str,
        digest: &'a str,
    },
    /// The cache namespace of the tenant of the run, before any task of the run.
    CacheNamespace(&'a str),
    Outputs {
        schema: &'a Value,
    },
    Sources(&'a [Source]),
    RemoteFiles(&'a [RemoteFile]),
    Delivered {
        target: &'a str,
        location: &'a str,
    },
    Replays {
        run: &'a str,
        task: u64,
    },
    /// The run ended; `stopped` when it was cancelled, which is not a failure of its own.
    Ended {
        error: Option<&'a str>,
        stopped: bool,
    },
}

impl Change<'_> {
    /// Whether the change is an event of the engine, which takes the next event number.
    pub fn event(&self) -> bool {
        matches!(
            self,
            Change::Requested { .. }
                | Change::Started { .. }
                | Change::Finished { .. }
                | Change::Validated { .. }
                | Change::Reused { .. }
                | Change::Marked { .. }
                | Change::Ended { .. }
        )
    }
}

/// Reads recorded runs. [`Database`] reads its file; a [`RunStore`] reads wherever it keeps runs.
pub trait RunReader {
    /// The latest runs first, at most `limit` of them.
    fn runs(&self, limit: u32) -> Result<Vec<Run>, DatabaseError>;
    fn run(&self, id: &str) -> Result<Option<Run>, DatabaseError>;
    /// The latest ended run of a workflow in a cache namespace, a real one when `real`.
    fn last_run(
        &self,
        workflow: &str,
        real: bool,
        namespace: Option<&str>,
    ) -> Result<Option<Run>, DatabaseError>;
    /// The runs of a lineage, the latest first.
    fn lineage(&self, lineage: &str) -> Result<Vec<String>, DatabaseError>;
    /// The cache namespace that a run ran in; `None` for the shared one.
    fn cache_namespace(&self, run: &str) -> Result<Option<String>, DatabaseError>;
    fn reusable(&self, runs: &[String]) -> Result<HashMap<String, Reusable>, DatabaseError>;
    fn run_inputs(&self, run: &str) -> Result<Option<RunInputs>, DatabaseError>;
    fn owner(&self, run: &str) -> Result<Option<Owner>, DatabaseError>;
    fn tasks(&self, run: &str) -> Result<Vec<TaskRow>, DatabaseError>;
    fn task_row(&self, run: &str, task: u64) -> Result<Option<TaskRow>, DatabaseError>;
    fn task(&self, run: &str, task: u64) -> Result<Option<TaskDetail>, DatabaseError>;
    fn task_attempts(&self, run: &str, task: u64) -> Result<Vec<Attempt>, DatabaseError>;
    fn attempts(&self, run: &str) -> Result<Vec<(u64, Attempt)>, DatabaseError>;
    fn files(&self, run: &str, task: Option<u64>) -> Result<Vec<FileRow>, DatabaseError>;
    fn edges(&self, run: &str) -> Result<Vec<EdgeRow>, DatabaseError>;
    fn markers(&self, run: &str) -> Result<Vec<MarkerRow>, DatabaseError>;
    fn log(
        &self,
        run: &str,
        task: u64,
        attempt: u32,
        stream: &str,
    ) -> Result<Option<Log>, DatabaseError>;
    fn events(&self, run: &str, after: i64, limit: u32) -> Result<Vec<Event>, DatabaseError>;
    fn last_event(&self, run: &str) -> Result<i64, DatabaseError>;
    fn deliveries(&self, run: &str) -> Result<Vec<Delivered>, DatabaseError>;
    fn sources(&self, run: &str) -> Result<Vec<Source>, DatabaseError>;
    fn remote_files(&self, run: &str) -> Result<Vec<RemoteFile>, DatabaseError>;
    fn images(&self, run: &str) -> Result<Vec<ImagePin>, DatabaseError>;
    /// The digests that earlier attempts pinned each image to.
    fn pins(&self) -> Result<HashMap<String, String>, DatabaseError>;
    fn graph(&self, run: &str) -> Result<Option<ExecutionGraph>, DatabaseError>;
    fn compare(&self, first: &str, second: &str) -> Result<Option<Comparison>, DatabaseError>;
    fn time_spent(&self, run: &str) -> Result<Option<TimeSpent>, DatabaseError>;
    fn why(&self, run: &str, task: u64) -> Result<Option<Why>, DatabaseError>;
    fn replays_of(&self, run: &str, task: u64) -> Result<Vec<Run>, DatabaseError>;
    /// The runs and output rows that made a location.
    fn producers(&self, location: &str) -> Result<Vec<(String, FileRow)>, DatabaseError>;
    /// The longest chain of work after each step in the earlier real runs of a workflow.
    fn chains(&self, workflow: &str) -> Result<HashMap<String, u64>, DatabaseError>;
}

/// Keeps runs for one or many coordinators, like SQLite, PostgreSQL or the API of a host. It
/// gets each change of a run as its [`crate::Recorder`] makes it, in order.
pub trait RunStore: RunReader + Send + Sync {
    /// Adds a new run, which joins the lineage of the run it resumes.
    fn begin(&self, run: &RunStart) -> Result<(), DatabaseError>;
    /// Applies one change to the run `run`; a stopped run takes none.
    fn apply(&self, run: &str, change: Change<'_>) -> Result<(), DatabaseError>;
    /// Stops a run whose owner ended without recording it, as [`Database::reconcile`] does.
    fn reconcile(&self, owner: &Owner, reason: &str) -> Result<bool, DatabaseError>;
}

/// The SQLite run database as a [`RunStore`], with one connection that its users share.
pub struct SqliteStore {
    database: Mutex<Database>,
}

impl SqliteStore {
    pub fn new(database: Database) -> SqliteStore {
        SqliteStore {
            database: Mutex::new(database),
        }
    }

    /// Opens the database file as [`Database::open`] does.
    pub fn open(path: &Path) -> Result<SqliteStore, DatabaseError> {
        Database::open(path).map(SqliteStore::new)
    }

    /// The connection, also after a thread panicked while it held it.
    pub fn lock(&self) -> MutexGuard<'_, Database> {
        self.database.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Runs in the memory of this process only, for tests and hosts that keep no runs. It uses
/// SQLite in memory, so that it reads exactly as the run database does.
pub struct MemoryStore {
    store: SqliteStore,
}

impl MemoryStore {
    pub fn new() -> Result<MemoryStore, DatabaseError> {
        let store = SqliteStore::new(Database::memory()?);
        Ok(MemoryStore { store })
    }

    pub fn lock(&self) -> MutexGuard<'_, Database> {
        self.store.lock()
    }
}

/// Reads through `$database`, an expression of `$this` that gives the database.
macro_rules! reader {
    ($type:ty, $this:ident => $database:expr) => {
        impl RunReader for $type {
            fn runs(&self, limit: u32) -> Result<Vec<Run>, DatabaseError> {
                let $this = self;
                Database::runs(&$database, limit)
            }
            fn run(&self, id: &str) -> Result<Option<Run>, DatabaseError> {
                let $this = self;
                Database::run(&$database, id)
            }
            fn last_run(
                &self,
                workflow: &str,
                real: bool,
                namespace: Option<&str>,
            ) -> Result<Option<Run>, DatabaseError> {
                let $this = self;
                Database::last_run(&$database, workflow, real, namespace)
            }
            fn lineage(&self, lineage: &str) -> Result<Vec<String>, DatabaseError> {
                let $this = self;
                Database::lineage(&$database, lineage)
            }
            fn cache_namespace(&self, run: &str) -> Result<Option<String>, DatabaseError> {
                let $this = self;
                Database::cache_namespace(&$database, run)
            }
            fn reusable(
                &self,
                runs: &[String],
            ) -> Result<HashMap<String, Reusable>, DatabaseError> {
                let $this = self;
                Database::reusable(&$database, runs)
            }
            fn run_inputs(&self, run: &str) -> Result<Option<RunInputs>, DatabaseError> {
                let $this = self;
                Database::run_inputs(&$database, run)
            }
            fn owner(&self, run: &str) -> Result<Option<Owner>, DatabaseError> {
                let $this = self;
                Database::owner(&$database, run)
            }
            fn tasks(&self, run: &str) -> Result<Vec<TaskRow>, DatabaseError> {
                let $this = self;
                Database::tasks(&$database, run)
            }
            fn task_row(&self, run: &str, task: u64) -> Result<Option<TaskRow>, DatabaseError> {
                let $this = self;
                Database::task_row(&$database, run, task)
            }
            fn task(&self, run: &str, task: u64) -> Result<Option<TaskDetail>, DatabaseError> {
                let $this = self;
                Database::task(&$database, run, task)
            }
            fn task_attempts(&self, run: &str, task: u64) -> Result<Vec<Attempt>, DatabaseError> {
                let $this = self;
                Database::task_attempts(&$database, run, task)
            }
            fn attempts(&self, run: &str) -> Result<Vec<(u64, Attempt)>, DatabaseError> {
                let $this = self;
                Database::attempts(&$database, run)
            }
            fn files(&self, run: &str, task: Option<u64>) -> Result<Vec<FileRow>, DatabaseError> {
                let $this = self;
                Database::files(&$database, run, task)
            }
            fn edges(&self, run: &str) -> Result<Vec<EdgeRow>, DatabaseError> {
                let $this = self;
                Database::edges(&$database, run)
            }
            fn markers(&self, run: &str) -> Result<Vec<MarkerRow>, DatabaseError> {
                let $this = self;
                Database::markers(&$database, run)
            }
            fn log(
                &self,
                run: &str,
                task: u64,
                attempt: u32,
                stream: &str,
            ) -> Result<Option<Log>, DatabaseError> {
                let $this = self;
                Database::log(&$database, run, task, attempt, stream)
            }
            fn events(
                &self,
                run: &str,
                after: i64,
                limit: u32,
            ) -> Result<Vec<Event>, DatabaseError> {
                let $this = self;
                Database::events(&$database, run, after, limit)
            }
            fn last_event(&self, run: &str) -> Result<i64, DatabaseError> {
                let $this = self;
                Database::last_event(&$database, run)
            }
            fn deliveries(&self, run: &str) -> Result<Vec<Delivered>, DatabaseError> {
                let $this = self;
                Database::deliveries(&$database, run)
            }
            fn sources(&self, run: &str) -> Result<Vec<Source>, DatabaseError> {
                let $this = self;
                Database::sources(&$database, run)
            }
            fn remote_files(&self, run: &str) -> Result<Vec<RemoteFile>, DatabaseError> {
                let $this = self;
                Database::remote_files(&$database, run)
            }
            fn images(&self, run: &str) -> Result<Vec<ImagePin>, DatabaseError> {
                let $this = self;
                Database::images(&$database, run)
            }
            fn pins(&self) -> Result<HashMap<String, String>, DatabaseError> {
                let $this = self;
                Database::pins(&$database)
            }
            fn graph(&self, run: &str) -> Result<Option<ExecutionGraph>, DatabaseError> {
                let $this = self;
                Database::graph(&$database, run)
            }
            fn compare(
                &self,
                first: &str,
                second: &str,
            ) -> Result<Option<Comparison>, DatabaseError> {
                let $this = self;
                Database::compare(&$database, first, second)
            }
            fn time_spent(&self, run: &str) -> Result<Option<TimeSpent>, DatabaseError> {
                let $this = self;
                Database::time_spent(&$database, run)
            }
            fn why(&self, run: &str, task: u64) -> Result<Option<Why>, DatabaseError> {
                let $this = self;
                Database::why(&$database, run, task)
            }
            fn replays_of(&self, run: &str, task: u64) -> Result<Vec<Run>, DatabaseError> {
                let $this = self;
                Database::replays_of(&$database, run, task)
            }
            fn producers(&self, location: &str) -> Result<Vec<(String, FileRow)>, DatabaseError> {
                let $this = self;
                Database::producers(&$database, location)
            }
            fn chains(&self, workflow: &str) -> Result<HashMap<String, u64>, DatabaseError> {
                let $this = self;
                Database::chains(&$database, workflow)
            }
        }
    };
}

reader!(Database, this => *this);
reader!(SqliteStore, this => this.lock());
reader!(MemoryStore, this => this.lock());

impl RunStore for SqliteStore {
    fn begin(&self, run: &RunStart) -> Result<(), DatabaseError> {
        crate::recorder::begin(&self.lock().connection, run)
    }

    fn apply(&self, run: &str, change: Change<'_>) -> Result<(), DatabaseError> {
        let database = self.lock();
        // Another connection may write the file too: taking the write lock before the reads
        // waits for it, where upgrading a read lock later fails at once.
        let transaction = rusqlite::Transaction::new_unchecked(
            &database.connection,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let stopped: bool = transaction.query_row(
            "SELECT state = 'stopped' FROM runs WHERE id = ?1",
            [run],
            |row| row.get(0),
        )?;
        if stopped {
            return Err(DatabaseError::Stopped(run.to_owned()));
        }
        let sequence = Database::last_event(&database, run)? + 1;
        crate::recorder::write(&transaction, run, sequence, &change)?;
        transaction.commit()?;
        Ok(())
    }

    fn reconcile(&self, owner: &Owner, reason: &str) -> Result<bool, DatabaseError> {
        self.lock().reconcile(owner, reason)
    }
}

impl RunStore for MemoryStore {
    fn begin(&self, run: &RunStart) -> Result<(), DatabaseError> {
        self.store.begin(run)
    }

    fn apply(&self, run: &str, change: Change<'_>) -> Result<(), DatabaseError> {
        self.store.apply(run, change)
    }

    fn reconcile(&self, owner: &Owner, reason: &str) -> Result<bool, DatabaseError> {
        self.store.reconcile(owner, reason)
    }
}
