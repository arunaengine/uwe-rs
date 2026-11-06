//! nf-databricks against a local fake of the Databricks SQL Statement Execution API: a polled
//! query in two result chunks, a statement, an insert with named parameters and an error.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// A request that the fake heard: the request line, the authorization and the body.
#[derive(Clone, Debug)]
struct Heard {
    line: String,
    token: String,
    body: serde_json::Value,
}

fn warehouse() -> (u16, Arc<Mutex<Vec<Heard>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("binds a free port");
    let port = listener.local_addr().expect("an address").port();
    let heard = Arc::new(Mutex::new(Vec::new()));
    let log = heard.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            let (mut length, mut token) = (0, String::new());
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
                    break;
                }
                let lower = header.to_ascii_lowercase();
                if let Some(value) = lower.strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
                if lower.starts_with("authorization:") {
                    token = header.trim().to_owned();
                }
            }
            let mut received = vec![0; length];
            let _ = reader.read_exact(&mut received);
            let body = serde_json::from_slice(&received).unwrap_or_default();
            let request = Heard {
                line: line.trim().trim_end_matches(" HTTP/1.1").to_owned(),
                token,
                body,
            };
            let reply = answer(&request);
            log.lock().expect("the log").push(request);
            let text = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{reply}",
                reply.len()
            );
            let _ = (&stream).write_all(text.as_bytes());
        }
    });
    (port, heard)
}

fn answer(request: &Heard) -> String {
    let statement = request.body["statement"].as_str().unwrap_or_default();
    let done = |columns: &str, rows: &str| {
        format!(
            r#"{{"statement_id":"s2","status":{{"state":"SUCCEEDED"}},"manifest":{{"schema":{{"columns":[{columns}]}}}},"result":{{"data_array":[{rows}]}}}}"#
        )
    };
    match request.line.as_str() {
        "POST /api/2.0/sql/statements/" if statement.starts_with("SELECT id") => {
            r#"{"statement_id":"s1","status":{"state":"PENDING"}}"#.to_owned()
        }
        "GET /api/2.0/sql/statements/s1" => r#"{"statement_id":"s1","status":{"state":"SUCCEEDED"},"manifest":{"schema":{"columns":[{"name":"id","type_name":"INT"},{"name":"name","type_name":"STRING"},{"name":"score","type_name":"DECIMAL"}]}},"result":{"data_array":[["1","a","1.50"]],"next_chunk_index":1}}"#.to_owned(),
        "GET /api/2.0/sql/statements/s1/result/chunks/1" => r#"{"data_array":[["2","b",null]]}"#.to_owned(),
        _ if statement.starts_with("SELECT * FROM ins") => {
            done(r#"{"name":"name","type_name":"STRING"},{"name":"len","type_name":"INT"}"#, "")
        }
        _ if statement.starts_with("insert") => {
            done(r#"{"name":"num_affected_rows","type_name":"LONG"}"#, r#"["1"]"#)
        }
        _ if statement.starts_with("SELECT bad") => r#"{"statement_id":"s3","status":{"state":"FAILED","error":{"error_code":"BAD_REQUEST","message":"[UNRESOLVED_COLUMN] bad"}}}"#.to_owned(),
        _ => r#"{"statement_id":"s4","status":{"state":"SUCCEEDED"}}"#.to_owned(),
    }
}

#[test]
fn speaks_statement_api() {
    let (port, heard) = warehouse();
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let config = format!(
        "plugins {{ id 'nf-databricks@0.1.0' }}\nsql {{ db {{ databricks {{\n  url = \
         'jdbc:databricks://127.0.0.1:{port}/default;transportMode=http;ssl=0;AuthMech=3;\
         httpPath=/sql/1.0/warehouses/w1;IgnoreTransactions=1'\n  user = 'token'\n  \
         password = 'dapi-test'\n}} }} }}\n"
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let script = "include { fromQuery; sqlInsert; sqlExecute } from 'plugin/nf-databricks'\n\
        workflow {\n  println 'create: ' + sqlExecute(db: 'databricks', statement: 'CREATE TABLE t (a INT)')\n  \
        println 'bad: ' + sqlExecute(db: 'databricks', statement: 'SELECT bad')\n  \
        channel.fromQuery('SELECT id, name, score FROM t', db: 'databricks').view { \"row ${it}\" }\n  \
        channel.of([\"it's\", 3]).sqlInsert(db: 'databricks', into: 'ins').view { \"passed ${it}\" }\n}\n";
    std::fs::write(folder.join("main.nf"), script).expect("writes the script");
    let (execution, printed) = common::run(folder, "databricks");
    assert!(
        execution.error.is_none(),
        "{:?}\n{printed}",
        execution.error
    );
    let mut lines: Vec<&str> = printed.lines().collect();
    lines.sort();
    let bad = "[UNRESOLVED_COLUMN] bad";
    let mut expected = vec![
        format!("ERROR ~ Error executing SQL statement: {bad}"),
        format!("bad: [success:false, error:{bad}]"),
        "create: [success:true, result:null]".to_owned(),
        "passed [it's, 3]".to_owned(),
        "row [1, a, 1.50]".to_owned(),
        "row [2, b, null]".to_owned(),
    ];
    expected.sort();
    assert_eq!(lines, expected);
    let heard = heard.lock().expect("the log").clone();
    assert!(
        heard
            .iter()
            .all(|request| request.token == "authorization: Bearer dapi-test")
    );
    let first = &heard[0].body;
    assert_eq!(first["warehouse_id"], "w1");
    assert_eq!(first["schema"], "default");
    assert_eq!(first["format"], "JSON_ARRAY");
    let insert = (heard.iter())
        .find(|request| {
            request.body["statement"]
                .as_str()
                .is_some_and(|sql| sql.starts_with("insert"))
        })
        .expect("the insert");
    assert_eq!(
        insert.body["statement"],
        "insert into ins ( name,len ) values ( :p0,:p1 )"
    );
    assert_eq!(
        insert.body["parameters"],
        serde_json::json!([{ "name": "p0", "value": "it's", "type": "STRING" },
            { "name": "p1", "value": "3", "type": "INT" }])
    );
}
