//! Records small runs and reads them back: runs, tasks, attempts, files, edges, logs, events,
//! resume lookups and the schema version.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use run_database::{Database, VERSION};

#[test]
fn a_database_of_a_newer_version_is_refused() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("runs.sqlite");
    let database = Database::open(&path).expect("creates");
    assert_eq!(database.version().expect("reads the version"), VERSION);
    drop(database);
    let connection = rusqlite::Connection::open(&path).expect("opens");
    connection
        .pragma_update(None, "user_version", VERSION + 1)
        .expect("sets a newer version");
    drop(connection);
    let error = Database::open(&path).err().expect("refused");
    assert!(error.to_string().contains("newer iunctus"), "{error}");
}
