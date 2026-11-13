//! nf-trino against a local fake Trino coordinator that answers by Trino's client protocol, and
//! the messages that nf-trino 0.1.0 printed with Nextflow 26.04.6.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

/// A request that the fake coordinator heard: method and path, headers and body.
#[derive(Clone, Debug)]
struct Heard {
    line: String,
    headers: Vec<(String, String)>,
    body: String,
}

/// Serves Trino's protocol on a free local port until the test ends; the requests it heard.
fn coordinator() -> (u16, Arc<Mutex<Vec<Heard>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("binds a free port");
    let port = listener.local_addr().expect("an address").port();
    let heard = Arc::new(Mutex::new(Vec::new()));
    let log = heard.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let request = read(&stream);
            let reply = answer(&request, port);
            log.lock().expect("the log").push(request);
            let mut stream = stream;
            let text = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{reply}",
                reply.len()
            );
            let _ = stream.write_all(text.as_bytes());
        }
    });
    (port, heard)
}

fn read(stream: &TcpStream) -> Heard {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let _ = reader.read_line(&mut line);
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.trim_end().split_once(": ") {
            headers.push((name.to_ascii_lowercase(), value.to_owned()));
        }
    }
    let length = (headers.iter())
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    let _ = reader.read_exact(&mut body);
    let line = line.trim_end().trim_end_matches(" HTTP/1.1").to_owned();
    let body = String::from_utf8_lossy(&body).into_owned();
    Heard {
        line,
        headers,
        body,
    }
}

/// Trino's answers: a query in two pages, a statement, the columns of `ins`, and an error.
fn answer(request: &Heard, port: u16) -> String {
    let next = |page: u32| format!("http://127.0.0.1:{port}/v1/statement/q1/{page}");
    match (request.line.as_str(), request.body.as_str()) {
        ("POST /v1/statement", body) if body.starts_with("SELECT id") => {
            format!(r#"{{"id":"q1","nextUri":"{}","stats":{{"state":"QUEUED"}}}}"#, next(1))
        }
        ("GET /v1/statement/q1/1", _) => format!(
            r#"{{"id":"q1","columns":[{{"name":"id","type":"integer"}},{{"name":"name","type":"varchar"}},{{"name":"score","type":"decimal(5,2)"}}],"data":[[1,"a","1.50"]],"nextUri":"{}"}}"#,
            next(2)
        ),
        ("GET /v1/statement/q1/2", _) => r#"{"id":"q1","data":[[2,"b",null]]}"#.to_owned(),
        ("POST /v1/statement", body) if body.starts_with("SHOW COLUMNS") => {
            r#"{"id":"q3","columns":[{"name":"Column","type":"varchar"},{"name":"Type","type":"varchar"}],"data":[["name","varchar"],["len","integer"]]}"#.to_owned()
        }
        ("POST /v1/statement", body) if body.starts_with("insert") => {
            r#"{"id":"q4","updateType":"INSERT","updateCount":1}"#.to_owned()
        }
        ("POST /v1/statement", body) if body.starts_with("SELECT bad") => {
            r#"{"id":"q5","error":{"message":"line 1:8: Column 'bad' cannot be resolved"}}"#
                .to_owned()
        }
        _ => r#"{"id":"q2","updateType":"CREATE TABLE"}"#.to_owned(),
    }
}

#[test]
fn speaks_trino_protocol() {
    let (port, heard) = coordinator();
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let config = format!(
        "plugins {{ id 'nf-trino@0.1.0' }}\nsql {{ db {{\n  trino {{ url = \
         'jdbc:trino://127.0.0.1:{port}/hive/default'; user = 'alice'; password = '' }}\n  \
         starburst {{ url = 'jdbc:starburst://127.0.0.1:{port}/hive/default'; user = 'alice' }}\n\
         }} }}\n"
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let script = "include { fromQuery; sqlInsert; sqlExecute } from 'plugin/nf-trino'\n\
        workflow {\n  println 'create: ' + sqlExecute(db: 'trino', statement: 'CREATE TABLE t (a INT)')\n  \
        println 'bad: ' + sqlExecute(db: 'trino', statement: 'SELECT bad')\n  \
        println 'name: ' + sqlExecute(db: 'trin', statement: 'x')\n  \
        println 'starburst: ' + sqlExecute(db: 'starburst', statement: 'x')\n  \
        channel.fromQuery('SELECT id, name, score FROM t', db: 'trino', emitColumns: true)\n    \
        .view { \"row ${it}\" }\n  \
        channel.of([\"it's\", 3]).sqlInsert(db: 'trino', into: 'ins').view { \"passed ${it}\" }\n}\n";
    std::fs::write(folder.join("main.nf"), script).expect("writes the script");
    let (execution, printed) = common::run(folder, "trino");
    assert!(
        execution.error.is_none(),
        "{:?}\n{printed}",
        execution.error
    );
    let starburst = format!("jdbc:starburst://127.0.0.1:{port}/hive/default");
    let bad = "line 1:8: Column 'bad' cannot be resolved";
    let mut lines: Vec<&str> = printed.lines().collect();
    lines.sort();
    let mut expected = vec![
        format!("ERROR ~ Error executing SQL statement: No suitable driver found for {starburst}"),
        format!("ERROR ~ Error executing SQL statement: {bad}"),
        format!("bad: [success:false, error:{bad}]"),
        "create: [success:true, result:null]".to_owned(),
        "name: [success:false, error:Unknown db name: trin - Did you mean: trino?]".to_owned(),
        "passed [it's, 3]".to_owned(),
        "row [1, a, 1.50]".to_owned(),
        "row [2, b, null]".to_owned(),
        "row [id, name, score]".to_owned(),
        format!("starburst: [success:false, error:No suitable driver found for {starburst}]"),
    ];
    expected.sort();
    assert_eq!(lines, expected);
    let heard = heard.lock().expect("the log").clone();
    let posted: Vec<&str> = (heard.iter())
        .filter(|request| request.line == "POST /v1/statement")
        .map(|request| request.body.as_str())
        .collect();
    assert!(
        posted.contains(&"CREATE TABLE t (a INT)"),
        "no `;` is added: {posted:?}"
    );
    assert!(
        posted.contains(&"insert into ins ( name,len ) values ( 'it''s',3 )"),
        "{posted:?}"
    );
    let first = &heard[0];
    let header = |name: &str| {
        let found = first.headers.iter().find(|(key, _)| key == name);
        found.map(|(_, value)| value.as_str())
    };
    assert_eq!(header("x-trino-user"), Some("alice"));
    assert_eq!(header("x-trino-catalog"), Some("hive"));
    assert_eq!(header("x-trino-schema"), Some("default"));
    assert_eq!(
        header("authorization"),
        None,
        "an empty password counts as unset"
    );
}
