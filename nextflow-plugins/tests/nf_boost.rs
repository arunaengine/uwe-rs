//! nf-boost's `request` against a local web server, and its `boost.cleanup` setting.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;

use nextflow_config::Config;
use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::value::Value;
use nextflow_plugins::{Context, call, catalogue, method};

struct Quiet;

impl Host for Quiet {
    fn print(&mut self, _: &str) {}
}

/// A web server on a free local port that answers each request with its method, path, body
/// and two headers as JSON; `POST` with 201, `/missing` with 404.
fn server(requests: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let address = listener.local_addr().expect("an address");
    std::thread::spawn(move || {
        for stream in listener.incoming().take(requests) {
            let mut stream = stream.expect("a connection");
            let mut reader = BufReader::new(stream.try_clone().expect("a reader"));
            let mut line = String::new();
            reader.read_line(&mut line).expect("a request line");
            let mut parts = line.split_whitespace();
            let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
            let mut headers = Vec::new();
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).expect("a header");
                match header.trim_end().split_once(": ") {
                    Some((name, value)) => headers.push((name.to_lowercase(), value.to_owned())),
                    None => break,
                }
            }
            let header = |name: &str| headers.iter().find(|(n, _)| n == name).map(|(_, v)| v);
            let length = header("content-length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let mut body = vec![0; length];
            reader.read_exact(&mut body).expect("the body");
            let answer = serde_json::json!({
                "method": method,
                "path": path,
                "body": String::from_utf8_lossy(&body),
                "content_type": header("content-type"),
                "x_test": header("x-test"),
            })
            .to_string();
            let status = match (method, path) {
                (_, "/missing") => "404 Not Found",
                ("POST", _) => "201 Created",
                _ => "200 OK",
            };
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                answer.len()
            );
            stream.write_all(head.as_bytes()).expect("writes");
            if method != "HEAD" {
                stream.write_all(answer.as_bytes()).expect("writes");
            }
        }
    });
    format!("http://{address}")
}

fn with_context<T>(run: impl FnOnce(&mut Context) -> T) -> T {
    let config = Config::default();
    let mut host = Quiet;
    let mut context = Context {
        host: &mut host,
        config: &config,
        params: Value::map([]),
        project_dir: "/",
        cli_params: &[],
        workflow: Value::Null,
    };
    run(&mut context)
}

/// The text of a result, or the exception of the failure.
fn shown(result: Result<Value, Error>) -> String {
    match result {
        Ok(value) => value.to_string(),
        Err(Error::Thrown { exception, .. }) => exception.to_string(),
        Err(other) => panic!("{other}"),
    }
}

fn member(object: &Value, name: &str, arguments: Vec<Value>) -> String {
    with_context(|context| {
        let found = method(object, name, arguments, context).map(|found| found.expect(name));
        shown(found)
    })
}

fn request(url: String, options: Vec<(&str, Value)>) -> Result<Value, Error> {
    let options: Vec<(Value, Value)> = (options.into_iter())
        .map(|(name, value)| (Value::string(name), value))
        .collect();
    let mut arguments = Vec::new();
    if !options.is_empty() {
        arguments.push(Value::map(options));
    }
    arguments.push(Value::string(url));
    with_context(|context| call("nf-boost", "request", arguments, context))
}

#[test]
fn sends_requests() {
    let base = server(4);
    let get = request(format!("{base}/get?x=1"), Vec::new()).expect("a connection");
    assert_eq!(
        get.java_class_name(),
        "sun.net.www.protocol.http.HttpURLConnection"
    );
    assert_eq!(member(&get, "getResponseCode", Vec::new()), "200");
    let stream = with_context(|context| method(&get, "getInputStream", Vec::new(), context));
    let Ok(Some(Value::Object(stream))) = stream else {
        panic!("a stream");
    };
    let text = match &stream.borrow().state {
        nextflow_evaluator::value::State::Stream(bytes, _) => {
            String::from_utf8_lossy(bytes).into_owned()
        }
        _ => panic!("a stream"),
    };
    assert_eq!(
        text,
        r#"{"body":"","content_type":null,"method":"GET","path":"/get?x=1","x_test":null}"#
    );
    let headers = Value::map([
        (
            Value::string("Content-Type"),
            Value::string("application/json"),
        ),
        (Value::string("X-Test"), Value::string("yes")),
    ]);
    let options = vec![
        ("method", Value::string("post")),
        ("body", Value::string("{\"k\": 1}")),
        ("headers", headers),
    ];
    let post = request(format!("{base}/post"), options).expect("a connection");
    assert_eq!(member(&post, "getResponseCode", Vec::new()), "201");
    assert_eq!(member(&post, "getResponseMessage", Vec::new()), "Created");
    assert_eq!(
        member(&post, "getContentType", Vec::new()),
        "application/json"
    );
    let head = request(
        format!("{base}/head"),
        vec![("method", Value::string("head"))],
    );
    let head = head.expect("a connection");
    assert_eq!(member(&head, "getRequestMethod", Vec::new()), "HEAD");
    assert_eq!(member(&head, "getResponseCode", Vec::new()), "200");
    let missing = request(format!("{base}/missing"), Vec::new()).expect("a connection");
    assert_eq!(
        member(&missing, "getInputStream", Vec::new()),
        format!("FileNotFoundException: {base}/missing")
    );
    assert_eq!(
        shown(request(
            format!("{base}/x"),
            vec![("method", Value::string("patch"))]
        )),
        "ProtocolException: Invalid HTTP method: PATCH"
    );
}

/// A config that loads this text.
fn loaded(text: &str) -> Config {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let file = folder.path().join("nextflow.config");
    std::fs::write(&file, text).expect("writes");
    let options = nextflow_config::Options {
        profiles: None,
        params: Value::map([]),
        project_dir: "/project".into(),
        launch_dir: "/launch".into(),
        work_dir: "/work".into(),
    };
    nextflow_config::load(&[file], &options, &mut Quiet).expect("loads")
}

#[test]
fn checks_cleanup_setting() {
    for value in ["true", "false", "'v1'", "'v2'"] {
        let config = loaded(&format!(
            "plugins {{ id 'nf-boost' }}\nboost.cleanup = {value}\n"
        ));
        let observers = catalogue::observers(&config).expect("a valid setting");
        assert!(observers.is_empty(), "{value}");
    }
    let config = loaded("plugins { id 'nf-boost' }\nboost.cleanup = 'v3'\n");
    let error = catalogue::observers(&config)
        .err()
        .expect("an invalid setting");
    assert_eq!(error, "Invalid `boost.cleanup` value -- v3");
}
