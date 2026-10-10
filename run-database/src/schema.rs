//! The tables of the run database, and how an older file is brought to the current version.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use rusqlite::{Connection, TransactionBehavior};

use crate::DatabaseError;

/// The version of the schema that this build writes. SQLite keeps it in `PRAGMA user_version`.
/// A change to the tables adds a step to [`MIGRATIONS`] and raises this number.
pub const VERSION: u32 = 8;

/// The steps from each version to the next: the first creates version 1 in an empty file.
const MIGRATIONS: [&str; VERSION as usize] = [
    VERSION_1, VERSION_2, VERSION_3, VERSION_4, VERSION_5, VERSION_6, VERSION_7, VERSION_8,
];

/// Version 1. Times are milliseconds since the Unix epoch (UTC). Text in JSON is marked as such.
/// A run is one `iunctus run`; its tasks are the tasks that the driver asked for; an attempt is
/// one execution of a task (retries add attempts).
const VERSION_1: &str = "
CREATE TABLE runs (
    id          TEXT PRIMARY KEY,
    -- The first run of a chain of resumed runs; a run that resumes no other has its own id.
    lineage     TEXT NOT NULL,
    -- The run this one resumes, if any.
    resumes     TEXT,
    -- The workflow file or directory, as an absolute path.
    workflow    TEXT NOT NULL,
    language    TEXT NOT NULL,
    -- The Git commit of the workflow's repository, when it is in one.
    revision    TEXT,
    -- The command line after `iunctus`, as a JSON array of strings.
    arguments   TEXT NOT NULL,
    -- The workflow parameters, as a JSON array of [name, value] pairs in the order given.
    parameters  TEXT NOT NULL,
    -- The executor, like `simulator` or `local`.
    executor    TEXT NOT NULL,
    -- 1 when the tasks did not really run: the simulator made up their files.
    simulated   INTEGER NOT NULL,
    -- Where a real run keeps its files, like a directory or `s3://bucket/prefix`.
    store       TEXT,
    -- The process that runs it, so that a reader can tell a stopped run from a running one.
    process     INTEGER NOT NULL,
    started     INTEGER NOT NULL,
    ended       INTEGER,
    -- `running`, `succeeded` or `failed`.
    state       TEXT NOT NULL,
    -- Why the run failed.
    error       TEXT
);
CREATE INDEX runs_workflow ON runs (workflow, started);
CREATE INDEX runs_lineage ON runs (lineage);

CREATE TABLE tasks (
    run         TEXT NOT NULL REFERENCES runs (id),
    -- The id that the driver gave the task, unique in its run.
    id          INTEGER NOT NULL,
    -- The process, rule, step or template that the task is an instance of.
    step        TEXT NOT NULL,
    name        TEXT NOT NULL,
    -- The task as the driver asked for it, as JSON: image, command, script, inputs, outputs.
    description TEXT NOT NULL,
    -- The SHA-256 of the description without the ids, and of the size and time of each local
    -- input: the same task with the same inputs in another run has the same fingerprint.
    fingerprint TEXT NOT NULL,
    requested   INTEGER NOT NULL,
    -- `waiting`, `running`, `succeeded`, `failed` after a process failure or semantic
    -- rejection, or `stopped` when the run ended first.
    state       TEXT NOT NULL,
    PRIMARY KEY (run, id)
);
CREATE INDEX tasks_fingerprint ON tasks (fingerprint);

CREATE TABLE attempts (
    run         TEXT NOT NULL,
    task        INTEGER NOT NULL,
    -- 1 for the first execution of the task, 2 for its first retry.
    number      INTEGER NOT NULL,
    started     INTEGER NOT NULL,
    ended       INTEGER,
    exit_code   INTEGER,
    -- Where the result came from when the task did not run: `cache` for the store, or the id
    -- of the earlier run that a resumed run took it from.
    reused      TEXT,
    -- The image as it ran, pinned to its digest, when the executor tells it.
    image       TEXT,
    -- 1 when the engine ran the task again after this attempt, for a failure of the machine.
    retried     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (run, task, number),
    FOREIGN KEY (run, task) REFERENCES tasks (run, id)
);

CREATE TABLE files (
    run         TEXT NOT NULL,
    task        INTEGER NOT NULL,
    -- The attempt that made an output; NULL for an input.
    attempt     INTEGER,
    -- `input` or `output`.
    role        TEXT NOT NULL,
    -- The path that the task sees, relative to its work directory unless absolute.
    path        TEXT NOT NULL,
    -- Where the file is: a local path, a location in the store, or a URL. Empty for an input
    -- with literal content.
    location    TEXT NOT NULL,
    directory   INTEGER NOT NULL,
    -- `sha256:<hex>`, of the file or of the manifest of a directory, when known.
    digest      TEXT,
    size        INTEGER,
    FOREIGN KEY (run, task) REFERENCES tasks (run, id)
);
CREATE INDEX files_task ON files (run, task);
CREATE INDEX files_location ON files (location);
CREATE INDEX files_digest ON files (digest);

CREATE TABLE edges (
    run         TEXT NOT NULL,
    -- The earlier task.
    source      INTEGER NOT NULL,
    -- The later task, which reads a file of the earlier one or waits for it.
    target      INTEGER NOT NULL,
    -- `file` or `order`.
    kind        TEXT NOT NULL,
    -- For a file edge, the path that the later task sees.
    path        TEXT,
    FOREIGN KEY (run, source) REFERENCES tasks (run, id),
    FOREIGN KEY (run, target) REFERENCES tasks (run, id)
);
CREATE INDEX edges_run ON edges (run, target);

CREATE TABLE markers (
    run         TEXT NOT NULL,
    -- The task whose outputs decide the tasks after it: a run with real data can have more
    -- or fewer tasks there than a simulated run.
    task        INTEGER NOT NULL,
    -- `content`, `wildcard` or `checkpoint`.
    kind        TEXT NOT NULL,
    -- Why, in one short sentence.
    reason      TEXT NOT NULL,
    FOREIGN KEY (run, task) REFERENCES tasks (run, id)
);
CREATE INDEX markers_run ON markers (run, task);

CREATE TABLE logs (
    run         TEXT NOT NULL,
    task        INTEGER NOT NULL,
    attempt     INTEGER NOT NULL,
    -- `stdout` or `stderr`.
    stream      TEXT NOT NULL,
    -- The SHA-256 of the whole text. A real run keeps the whole text in its store under it.
    digest      TEXT NOT NULL,
    -- The size of the whole text in bytes.
    size        INTEGER NOT NULL,
    -- The whole text, or its end when it is longer than the limit of the recorder.
    text        TEXT NOT NULL,
    PRIMARY KEY (run, task, attempt, stream),
    FOREIGN KEY (run, task, attempt) REFERENCES attempts (run, task, number)
);

CREATE TABLE events (
    run         TEXT NOT NULL REFERENCES runs (id),
    -- Counts from 1 in each run, in the order the engine saw the events.
    sequence    INTEGER NOT NULL,
    time        INTEGER NOT NULL,
    -- `requested`, `started`, `finished`, `reused` or `marked` for a task, `ended` for the run.
    kind        TEXT NOT NULL,
    task        INTEGER,
    attempt     INTEGER,
    PRIMARY KEY (run, sequence)
);
";

/// Version 2: the inputs of each run (decision 24), as JSON: the JSON Schema of the inputs of its
/// workflow, and the values that the run checked against it. Both are NULL for a run whose driver
/// does not describe its inputs.
const VERSION_2: &str = "
ALTER TABLE runs ADD COLUMN input_schema TEXT;
ALTER TABLE runs ADD COLUMN inputs TEXT;
";

/// Version 3: what an attempt used, as the cgroup of its container counted it, when the
/// executor measured it: CPU time in microseconds, the peak of memory in bytes (page cache
/// included), and the bytes of block I/O.
const VERSION_3: &str = "
ALTER TABLE attempts ADD COLUMN cpu_usec INTEGER;
ALTER TABLE attempts ADD COLUMN memory_peak INTEGER;
ALTER TABLE attempts ADD COLUMN read_bytes INTEGER;
ALTER TABLE attempts ADD COLUMN write_bytes INTEGER;
";

/// Version 4: what a run needs to be repeated exactly, and the verdicts of the reproducibility
/// check.
const VERSION_4: &str = "
ALTER TABLE runs ADD COLUMN program TEXT;
ALTER TABLE runs ADD COLUMN launch TEXT;
ALTER TABLE runs ADD COLUMN workflow_folder TEXT;
ALTER TABLE runs ADD COLUMN repeats TEXT;
ALTER TABLE runs ADD COLUMN output_schema TEXT;

CREATE TABLE sources (
    run         TEXT NOT NULL REFERENCES runs (id),
    -- `workflow` for a file of the folder of the workflow, `named` for a file that the command
    -- line names, like a config file, a params file or a CWL job file.
    kind        TEXT NOT NULL,
    -- A file of the workflow relative to its folder; a named file as an absolute path.
    path        TEXT NOT NULL,
    -- `sha256:<hex>` of the file as the run found it. A real run keeps the content in its store.
    digest      TEXT NOT NULL,
    size        INTEGER NOT NULL
);
CREATE INDEX sources_run ON sources (run);

CREATE TABLE remote_files (
    run         TEXT NOT NULL REFERENCES runs (id),
    -- The address the run read, like `https://...` or `s3://bucket/key`.
    address     TEXT NOT NULL,
    -- `sha256:<hex>` of the content the run read. A real run keeps the content in its store.
    digest      TEXT NOT NULL,
    size        INTEGER NOT NULL,
    -- When the content was read from the address; an earlier run may have read it.
    fetched     INTEGER NOT NULL,
    -- The version that the provider gave then, like an ETag or a time of change, when known.
    version     TEXT,
    PRIMARY KEY (run, address)
);

CREATE TABLE checks (
    run         TEXT NOT NULL,
    task        INTEGER NOT NULL,
    attempt     INTEGER NOT NULL,
    -- The run that made the result in the cache that the task was compared with.
    earlier     TEXT NOT NULL,
    -- `same`, `differs` or `failed`.
    verdict     TEXT NOT NULL,
    -- The outputs whose digests differ, as a JSON array of paths.
    differing   TEXT NOT NULL,
    -- The exit code of a task that failed when it ran again.
    exit_code   INTEGER,
    PRIMARY KEY (run, task, attempt),
    FOREIGN KEY (run, task) REFERENCES tasks (run, id)
);
";

/// Version 5: replays of one task. A replay is a run of its own, of one task of an earlier run,
/// which it names; resume and `why` leave replays out.
const VERSION_5: &str = "
ALTER TABLE runs ADD COLUMN replays TEXT;
ALTER TABLE runs ADD COLUMN replays_task INTEGER;
";

/// Version 6: where the results of a run went, like the files of a Nextflow `publishDir`, a
/// Snakemake output in the folder of the workflow or the outputs of a CWL workflow.
const VERSION_6: &str = "
CREATE TABLE deliveries (
    run         TEXT NOT NULL REFERENCES runs (id),
    -- Where the result went: a path on this machine or an address of a storage provider.
    target      TEXT NOT NULL,
    -- The output that went there, by its location in the store.
    location    TEXT NOT NULL,
    PRIMARY KEY (run, target)
);
";

const VERSION_7: &str = "
ALTER TABLE runs ADD COLUMN process_host TEXT;
ALTER TABLE runs ADD COLUMN process_boot TEXT;
ALTER TABLE runs ADD COLUMN process_start TEXT;
";

/// Version 8: whether the driver explicitly accepted an attempt's process result. NULL keeps
/// the unknown verdict of rows written before acceptance was recorded.
const VERSION_8: &str = "
ALTER TABLE attempts ADD COLUMN accepted INTEGER CHECK (accepted IN (0, 1));
";

/// Brings the schema of a database to [`VERSION`]: creates it in a new file, and runs the
/// missing steps on an older one, in one transaction.
pub(crate) fn migrate(connection: &mut Connection) -> Result<(), DatabaseError> {
    let version = |connection: &Connection| -> Result<u32, DatabaseError> {
        let found: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if found > VERSION {
            return Err(DatabaseError::Newer {
                found,
                known: VERSION,
            });
        }
        Ok(found)
    };
    if version(connection)? == VERSION {
        return Ok(());
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let found = version(&transaction)?;
    for step in &MIGRATIONS[found as usize..] {
        transaction.execute_batch(step)?;
    }
    transaction.pragma_update(None, "user_version", VERSION)?;
    transaction.commit()?;
    Ok(())
}
