//! nf-sqldb against the runs that nf-sqldb 0.7.1 recorded with Nextflow 26.04.6 on SQLite,
//! and against PostgreSQL in a local container.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const INCLUDE: &str = "include { fromQuery; sqlInsert; sqlExecute } from 'plugin/nf-sqldb'\n";

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

fn owned(rows: &[&str]) -> Vec<String> {
    rows.iter().map(|row| row.to_string()).collect()
}

#[test]
fn inserts_items() {
    let cases = [
        (
            "channel.of(['a',1],['b',2]).sqlInsert(into: 'ins', columns: ['NAME','LEN'], db: 'foo').view()",
            "[a, 1]\n[b, 2]\n",
            owned(&["a|1", "b|2"]),
        ),
        (
            "channel.of([NAME:'m', LEN:3],[LEN:4, NAME:'n']).sqlInsert(into: 'ins', db: 'foo').view()",
            "[NAME:m, LEN:3]\n[LEN:4, NAME:n]\n",
            owned(&["m|3", "n|4"]),
        ),
        (
            "channel.of([name:'m', len:3]).sqlInsert(into: 'ins', db: 'foo').view()",
            "[name:m, len:3]\n",
            owned(&["|"]),
        ),
        (
            "channel.of([NAME:'k', LEN:5, EXTRA:1]).sqlInsert(into: 'ins', columns: 'NAME,LEN', db: 'foo').view()",
            "[NAME:k, LEN:5, EXTRA:1]\n",
            owned(&["k|5"]),
        ),
        (
            "channel.of('only').sqlInsert(into: 'ins', columns: 'NAME', db: 'foo').view()",
            "only\n",
            owned(&["only|"]),
        ),
        (
            "channel.value(['v',1]).sqlInsert(into: 'ins', columns: 'NAME,LEN', db: 'foo').view()",
            "[v, 1]\n",
            owned(&["v|1"]),
        ),
        (
            "channel.of(['x',9]).sqlInsert(into: 'ins', db: 'foo').view()",
            "[x, 9]\n",
            owned(&["x|9"]),
        ),
        (
            "channel.of(['a',1]).sqlInsert(into: 'ins', columns: 'NAME,LEN', batchSize: 0, db: 'foo').view()",
            "[a, 1]\n",
            owned(&["a|1"]),
        ),
        (
            "channel.of('single').sqlInsert(statement: 'insert into ins(NAME) values (?)', db: 'foo', batchSize: 1).view { \"single ${it}\" }",
            "single single\n",
            owned(&["single|"]),
        ),
        (
            "channel.of(['a',1]).sqlInsert(into: 'ins', columns: 'NAME,LEN', batch: 5, db: 'foo').view()",
            "WARN: The option 'batch' for the 'sqlInsert' operator has been deprecated - Use \
             'batchSize' instead\n[a, 1]\n",
            owned(&["a|1"]),
        ),
        (
            "channel.of(1..25).map { [\"n$it\", it] }.sqlInsert(into: 'ins', db: 'foo', batchSize: 7).count().view()",
            "25\n",
            (1..=25).map(|n| format!("n{n}|{n}")).collect(),
        ),
    ];
    for (body, printed_text, rows) in cases {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let folder = directory.path();
        let url = database(folder);
        let (error, printed) = run(folder, &url, body);
        assert_eq!(error, None, "{body}\n{printed}");
        assert_eq!(printed, printed_text, "{body}");
        assert_eq!(table(folder, "ins"), rows, "{body}");
    }
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let url = database(folder);
    let body = "channel.of(['a',1]).sqlInsert(into: 'newt', columns: 'NAME,LEN', setup: 'CREATE \
                TABLE IF NOT EXISTS newt (NAME TEXT, LEN INT)', db: 'foo').view()";
    let (error, _) = run(folder, &url, body);
    assert_eq!(error, None);
    assert_eq!(table(folder, "newt"), ["a|1"]);
}

#[test]
fn reports_errors() {
    let cases = [
        (
            "channel.of(['a',1]).sqlInsert(into: 'ins', foo: 1, db: 'foo').view()",
            "Unknown argument 'foo' for operator 'sqlInsert' -- Possible arguments: db, into, \
             columns, statement, batch, batchSize, setup",
        ),
        (
            "channel.of(['a',1]).sqlInsert(columns: 'NAME,LEN', db: 'foo').view()",
            "Missing sqlInsert target table - Use 'into' option to specify it",
        ),
        (
            "channel.of(null).sqlInsert(into: 'ins', columns: 'NAME', db: 'foo').view()",
            "Invalid sqlInsert entry - It must be a non-null value",
        ),
        (
            "channel.of(['a',1,2]).sqlInsert(into: 'ins', columns: 'NAME,LEN', db: 'foo').view()",
            "Index 2 out of bounds for length 2",
        ),
        (
            "channel.of([NAME:'m', FOO:3]).sqlInsert(into: 'ins', db: 'foo').view()",
            "Unknown columns for table 'ins': FOO - Offending record: [NAME:m, FOO:3]",
        ),
        (
            "channel.of(['a',1]).sqlInsert(into: 'ins', columns: 'NAME,LEN', db: 'nope').view()",
            "Unknown db name: nope - Available databases: foo",
        ),
        (
            "channel.of(['a',1]).sqlInsert(into: 'ins', columns: 'NAME,LEN', batchSize: -1, db: 'foo').view()",
            "SQL batch option must be greater than zero: -1",
        ),
        (
            "channel.of(['a',1]).sqlInsert(into: 'ins', columns: 5, db: 'foo').view()",
            "Value '5' cannot be used in in parameter 'columns' for operator 'sqlInsert' -- \
             Possible values: interface java.lang.CharSequence, interface java.util.List",
        ),
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
        (
            "channel.fromQuery('select 1 limit 1', db: 'foo', batchSize: 2).view()",
            "Sql query should not include the LIMIT statement when pageSize is specified",
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

#[test]
fn queries_rows() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let url = database(folder);
    let body = "  channel.fromQuery('select id, name, value, flag, n, d from sample order by id', \
                db: 'foo').view { r -> \"row ${r} ${r.collect { it?.getClass()?.simpleName }}\" }
  channel.fromQuery('select id, name from sample order by id', db: 'foo', emitColumns: \
                true).view { \"cols ${it}\" }
  channel.fromQuery('select id from sample order by id; -- all\\n', db: 'foo', batchSize: 2, \
                batchDelay: 0).view { \"batch ${it}\" }
  channel.fromQuery('select rate_limit from (select 1 as rate_limit)', db: 'foo', batchSize: \
                1, batchDelay: 0).view { \"limit ${it}\" }";
    let (error, printed) = run(folder, &url, body);
    assert_eq!(error, None, "{printed}");
    let mut lines: Vec<&str> = printed.lines().collect();
    lines.sort();
    assert_eq!(
        lines,
        [
            "batch [1]",
            "batch [2]",
            "batch [3]",
            "cols [1, alpha]",
            "cols [2, beta]",
            "cols [3, gamma]",
            "cols [id, name]",
            "limit [1]",
            "row [1, alpha, 10.5, 1, 9007199254740993, 2024-01-02] [Integer, String, Double, \
             Integer, Long, String]",
            "row [2, beta, null, 0, 2, null] [Integer, String, null, Integer, Integer, null]",
            "row [3, gamma, 3.0, null, 3, 2024-03-04] [Integer, String, Double, null, Integer, \
             String]",
        ]
    );
}

/// A container that `docker rm -f` stops when the test ends.
struct Container(String);

impl Drop for Container {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "-f", &self.0]).output();
    }
}

/// PostgreSQL 17 in a container on a random local port, only when Docker has the image; the
/// test never pulls it.
fn postgres() -> Option<(Container, u16)> {
    let image = "postgres:17";
    let present = Command::new("docker")
        .args(["image", "inspect", image])
        .output();
    if !present.is_ok_and(|output| output.status.success()) {
        eprintln!("skipped: Docker or its {image} image is not available");
        return None;
    }
    let started = Command::new("docker")
        .args([
            "run",
            "-d",
            "--rm",
            "--pull",
            "never",
            "-e",
            "POSTGRES_PASSWORD=secret",
        ])
        .args(["-p", "127.0.0.1::5432", image])
        .output()
        .expect("runs docker");
    assert!(started.status.success(), "{started:?}");
    let container = Container(String::from_utf8_lossy(&started.stdout).trim().to_owned());
    let port = Command::new("docker")
        .args(["port", &container.0, "5432/tcp"])
        .output()
        .expect("runs docker");
    let text = String::from_utf8_lossy(&port.stdout);
    let port = text
        .trim()
        .rsplit(':')
        .next()
        .and_then(|port| port.parse().ok());
    let port = port.expect("a published port");
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let ready = Command::new("docker")
            .args([
                "exec",
                &container.0,
                "pg_isready",
                "-h",
                "127.0.0.1",
                "-U",
                "postgres",
            ])
            .output()
            .is_ok_and(|output| output.status.success());
        if ready {
            return Some((container, port));
        }
        assert!(Instant::now() < deadline, "PostgreSQL did not start");
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[test]
fn uses_postgres() {
    let Some((_container, port)) = postgres() else {
        return;
    };
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let config = format!(
        "plugins {{ id 'nf-sqldb@0.7.1' }}\nsql {{ db {{ pg {{\n  url = \
         'jdbc:postgresql://127.0.0.1:{port}/postgres?sslmode=disable'\n  user = 'postgres'\n  \
         password = 'secret'\n}} }} }}\n"
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let script = format!(
        "{INCLUDE}workflow {{\n  println sqlExecute(db: 'pg', statement: 'CREATE TABLE t (id \
         INT, name TEXT, score NUMERIC(6,2), ok BOOLEAN, day DATE, at TIMESTAMP, big BIGINT)')\n  \
         println sqlExecute(db: 'pg', statement: \"INSERT INTO t VALUES (1, 'a', 12.34, true, \
         '2024-01-02', '2024-01-02 03:04:05.12', 9007199254740993)\")\n  \
         channel.of([2, 'b'], [3, 'c']).sqlInsert(db: 'pg', into: 't', columns: 'id, \
         name').count().view {{ \"inserted ${{it}}\" }}\n  \
         channel.fromQuery('select * from t where id = 1', db: 'pg', emitColumns: true)\n    \
         .view {{ r -> \"row ${{r}} ${{r.collect {{ it?.getClass()?.simpleName }}}}\" }}\n}}\n"
    );
    std::fs::write(folder.join("main.nf"), script).expect("writes the script");
    let (execution, printed) = common::run(folder, "postgres");
    assert!(
        execution.error.is_none(),
        "{:?}\n{printed}",
        execution.error
    );
    let mut lines: Vec<&str> = printed.lines().collect();
    lines.sort();
    assert_eq!(
        lines,
        [
            "[success:true, result:1]",
            "[success:true, result:null]",
            "inserted 2",
            "row [1, a, 12.34, true, 2024-01-02, 2024-01-02 03:04:05.12, 9007199254740993] \
             [Integer, String, BigDecimal, Boolean, Date, Timestamp, Long]",
            "row [id, name, score, ok, day, at, big] [String, String, String, String, String, \
             String, String]",
        ]
    );
}
