//! A fake HTTP server on 127.0.0.1 for the service plugin tests: it answers from routes in
//! the format of the recorded fixtures and keeps every request.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
#![allow(dead_code, reason = "each test file uses only a part of this module")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// A request that the server got.
#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        (self.headers.iter())
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// The server, which stops when dropped.
pub struct Fake {
    pub address: String,
    requests: Arc<Mutex<Vec<Request>>>,
    stopped: Arc<AtomicBool>,
}

impl Fake {
    /// Answers like the routes `[{"method", "match", "status", "type", "body"}]` of the
    /// recorded fixtures: the first route whose `match` is in the path, else 200 and `{}`.
    pub fn start(routes: serde_json::Value) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binds a free port");
        let address = format!("http://{}", listener.local_addr().expect("an address"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stopped = Arc::new(AtomicBool::new(false));
        let (kept, stop) = (requests.clone(), stopped.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let (kept, routes) = (kept.clone(), routes.clone());
                std::thread::spawn(move || serve(stream, &routes, &kept));
            }
        });
        Fake {
            address,
            requests,
            stopped,
        }
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().expect("not poisoned").clone()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address.trim_start_matches("http://"));
    }
}

/// Answers the requests of one connection until the client closes it.
fn serve(stream: TcpStream, routes: &serde_json::Value, kept: &Mutex<Vec<Request>>) {
    let mut writer = stream.try_clone().expect("clones the stream");
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_owned();
        let path = parts.next().unwrap_or_default().to_owned();
        let mut headers = Vec::new();
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).unwrap_or(0) == 0 {
                return;
            }
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':') {
                headers.push((name.trim().to_owned(), value.trim().to_owned()));
            }
        }
        let length = (headers.iter())
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = vec![0; length];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        let request = Request {
            method,
            path,
            headers,
            body,
        };
        let (status, kind, payload) = answer(routes, &request);
        kept.lock().expect("not poisoned").push(request);
        let head = format!(
            "HTTP/1.1 {status} Fake\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\r\n",
            payload.len()
        );
        if writer.write_all(head.as_bytes()).is_err() || writer.write_all(&payload).is_err() {
            return;
        }
    }
}

fn answer(routes: &serde_json::Value, request: &Request) -> (u64, String, Vec<u8>) {
    let routes = routes.as_array().cloned().unwrap_or_default();
    for route in routes {
        let method = route["method"].as_str().unwrap_or(&request.method);
        let found = route["match"]
            .as_str()
            .is_some_and(|text| request.path.contains(text));
        if method == request.method && found {
            let status = route["status"].as_u64().unwrap_or(200);
            let kind = route["type"]
                .as_str()
                .unwrap_or("application/json")
                .to_owned();
            let body = match &route["body"] {
                serde_json::Value::String(text) => text.clone().into_bytes(),
                other => other.to_string().into_bytes(),
            };
            return (status, kind, body);
        }
    }
    (200, "application/json".to_owned(), b"{}".to_vec())
}

/// The requests of a recorded fixture log, one JSON line each: method, path and body.
pub fn recorded(log: &str) -> Vec<serde_json::Value> {
    (log.lines())
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .collect()
}

/// The keys of an object at every depth as sorted `.a.b` paths; list items share one path.
pub fn keys(value: &serde_json::Value) -> Vec<String> {
    let mut found = Vec::new();
    walk(value, "", &mut found);
    found.sort();
    found.dedup();
    found
}

fn walk(value: &serde_json::Value, path: &str, found: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(entries) => {
            for (key, value) in entries {
                let inner = format!("{path}.{key}");
                found.push(inner.clone());
                walk(value, &inner, found);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                walk(item, path, found);
            }
        }
        _ => {}
    }
}
