//! nf-sqldb against the runs that nf-sqldb 0.7.1 recorded with Nextflow 26.04.6 on SQLite.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::path::Path;

const INCLUDE: &str = "include { sqlExecute } from 'plugin/nf-sqldb'\n";

/// Runs a workflow body with the database `foo` at `url`; gives the error and the printed text.
fn run(folder: &Path, url: &str, body: &str) -> (Option<String>, String) {
    let config = format!(
        "plugins {{ id 'nf-sqldb@0.7.1' }}\nsql {{\n  db {{\n    foo {{ url = '{url}' }}\n  }}\n}}\n"
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let script = format!("{INCLUDE}workflow {{\n{body}\n}}\n");
    std::fs::write(folder.join("main.nf"), script).expect("writes the script");
    let (execution, printed) = common::run(folder, "sqldb");
    (execution.error.map(|error| format!("{error:?}")), printed)
}

/// The SQLite file of the recording, with its `sample` and `ins` tables.
fn database(folder: &Path) -> String {
    let path = folder.join("test.db");
    let connection = rusqlite::Connection::open(&path).expect("opens");
    connection
        .execute_batch(
            "CREATE TABLE sample(id INTEGER PRIMARY KEY, name TEXT, value REAL, flag BOOLEAN, \
             n BIGINT, d DATE, blob BLOB);
             INSERT INTO sample VALUES (1,'alpha',10.5,1,9007199254740993,'2024-01-02',x'0102');
             INSERT INTO sample VALUES (2,'beta',NULL,0,2,NULL,NULL);
             INSERT INTO sample VALUES (3,'gamma',3.0,NULL,3,'2024-03-04',NULL);
             CREATE TABLE ins(NAME TEXT, LEN INTEGER);",
        )
        .expect("fills the database");
    format!("jdbc:sqlite:{}", path.display())
}

/// The rows of a table as `a|b` lines, null as empty, as the `sqlite3` program prints them.
fn table(folder: &Path, name: &str) -> Vec<String> {
    let connection = rusqlite::Connection::open(folder.join("test.db")).expect("opens");
    let mut statement = (connection.prepare(&format!("SELECT * FROM {name}"))).expect("prepares");
    let width = statement.column_count();
    let rows = statement.query_map([], |row| {
        let cells: Vec<String> = (0..width)
            .map(|index| match row.get_ref(index) {
                Ok(rusqlite::types::ValueRef::Text(text)) => {
                    String::from_utf8_lossy(text).into_owned()
                }
                Ok(rusqlite::types::ValueRef::Integer(value)) => value.to_string(),
                _ => String::new(),
            })
            .collect();
        Ok(cells.join("|"))
    });
    rows.expect("reads")
        .map(|row| row.expect("a row"))
        .collect()
}

#[test]
fn executes_statements() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let url = database(folder);
    let body = "  println 'ddl: ' + sqlExecute(db: 'foo', statement: 'CREATE TABLE IF NOT EXISTS \
                t2 (id INTEGER, name TEXT)')
  println 'ddl-multiline: ' + sqlExecute(db: 'foo', statement: '''
     CREATE TABLE IF NOT EXISTS t3 (id INTEGER)''')
  println 'insert: ' + sqlExecute(db: 'foo', statement: \"INSERT INTO t2 VALUES (1,'a'),(2,'b')\")
  println 'update: ' + sqlExecute(db: 'foo', statement: \"UPDATE t2 SET name='z'\")
  println 'delete-none: ' + sqlExecute(db: 'foo', statement: 'DELETE FROM t2 WHERE id > 99;')
  println 'bad-sql: ' + sqlExecute(db: 'foo', statement: 'INSERT INTO nope VALUES (1)')
  println 'unknown-db: ' + sqlExecute(db: 'fo', statement: 'select 1')
  println 'unknown-db2: ' + sqlExecute(db: 'zzz', statement: 'select 1')
  println 'no-statement: ' + sqlExecute(db: 'foo', statement: '')
  println 'select: ' + sqlExecute(db: 'foo', statement: 'select * from t2')
  println 'multiline: ' + sqlExecute(db: 'foo', statement: 'CREATE TABLE ml (\\n  a INT\\n)')
  println 'positional: ' + sqlExecute([db: 'foo', statement: 'DELETE FROM ins'])";
    let (error, printed) = run(folder, &url, body);
    assert_eq!(error, None, "{printed}");
    let missing = "[SQLITE_ERROR] SQL error or missing database (no such table: nope)";
    let expected = format!(
        "ddl: [success:true, result:null]\nddl-multiline: [success:true, result:null]\n\
         insert: [success:true, result:2]\nupdate: [success:true, result:2]\n\
         delete-none: [success:true, result:0]\n\
         ERROR ~ Error executing SQL statement: {missing}\n\
         bad-sql: [success:false, error:{missing}]\n\
         unknown-db: [success:false, error:Unknown db name: fo - Did you mean: foo?]\n\
         unknown-db2: [success:false, error:Unknown db name: zzz - Did you mean: foo?]\n\
         no-statement: [success:false, error:Missing required parameter 'statement']\n\
         select: [success:true, result:0]\nmultiline: [success:true, result:0]\n\
         positional: [success:true, result:0]\n"
    );
    assert_eq!(printed, expected);
    assert_eq!(table(folder, "t2"), ["1|z", "2|z"]);
}

#[test]
fn reports_errors() {
    let cases = [
        (
            "println sqlExecute(db: 'foo', statement: 'select 1', foo: 2)",
            "Unknown argument 'foo' for operator 'sqlExecute' -- Possible arguments: db, statement",
        ),
        (
            "println sqlExecute(db: \"foo\", statement: 42)",
            "Value '42' cannot be used in in parameter 'statement' for operator 'sqlExecute' -- \
             Value don't match: interface java.lang.CharSequence",
        ),
        (
            "println sqlExecute(statement: 'CREATE TABLE x (a INT)')",
            "H2 is a Java database, which uwe cannot run: jdbc:h2:mem:",
        ),
    ];
    for (body, message) in cases {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let folder = directory.path();
        let url = database(folder);
        let (error, printed) = run(folder, &url, body);
        let error = error.unwrap_or_default();
        assert!(error.contains(message), "{body}\n{error}\n{printed}");
        assert!(table(folder, "ins").is_empty(), "{body}");
    }
    let directory = tempfile::tempdir().expect("a temporary directory");
    let (error, _) = run(
        directory.path(),
        "mysql://x",
        "println sqlExecute(db: 'foo', statement: 'x')",
    );
    let error = error.unwrap_or_default();
    assert!(
        error.contains("Invalid database JDBC connection url: mysql://x"),
        "{error}"
    );
}
