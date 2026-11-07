//! A web service for tests on 127.0.0.1 that answers each request through a function and keeps
//! the requests, for fakes of REST services.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

/// A request as the service read it.
#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    /// The path with its query, like `/a?b=c`.
    pub path: String,
    /// The headers, by lower-case name.
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn json(status: u16, value: serde_json::Value) -> Reply {
        Reply {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: value.to_string().into_bytes(),
        }
    }
}

pub struct Service {
    /// Like `http://127.0.0.1:4711`.
    pub address: String,
    pub requests: Arc<Mutex<Vec<Request>>>,
}

impl Service {
    /// Answers each request with `answer`, which gets the request and the address of the
    /// service, for links back to it.
    pub fn start(answer: impl Fn(&Request, &str) -> Reply + Send + 'static) -> Service {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
        let address = format!("http://{}", listener.local_addr().expect("an address"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (seen, base) = (requests.clone(), address.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let Some(request) = read(&stream) else {
                    continue;
                };
                seen.lock().expect("the requests").push(request.clone());
                write(stream, answer(&request, &base));
            }
        });
        Service { address, requests }
    }

    /// The requests so far, as `METHOD /path`.
    pub fn lines(&self) -> Vec<String> {
        let requests = self.requests.lock().expect("the requests");
        (requests.iter())
            .map(|request| format!("{} {}", request.method, request.path))
            .collect()
    }
}

fn read(stream: &TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut first = String::new();
    reader.read_line(&mut first).ok()?;
    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.trim().split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let length = (headers.get("content-length"))
        .and_then(|length| length.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    let mut parts = first.split_whitespace();
    Some(Request {
        method: parts.next()?.to_owned(),
        path: parts.next()?.to_owned(),
        headers,
        body,
    })
}

fn write(mut stream: TcpStream, reply: Reply) {
    let mut head = format!(
        "HTTP/1.1 {} Answer\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.status,
        reply.body.len()
    );
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&reply.body);
}
