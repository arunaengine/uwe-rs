//! A small web server for tests: it serves files from memory on 127.0.0.1, may ask for one
//! header, like a token, and counts the requests it answers.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub struct WebServer {
    /// The address of the server, like `http://127.0.0.1:4711`.
    pub address: String,
    /// The requests so far, like `GET /a.txt`.
    pub requests: Arc<Mutex<Vec<String>>>,
    served: Arc<AtomicUsize>,
}

impl WebServer {
    /// Serves `files` by path. With `header`, a request without that header and value gets
    /// 401 Unauthorized.
    pub fn start(files: &[(&str, &str)], header: Option<(&str, &str)>) -> WebServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
        let address = format!("http://{}", listener.local_addr().expect("an address"));
        let files: HashMap<String, Vec<u8>> = files
            .iter()
            .map(|(path, body)| (path.to_string(), body.as_bytes().to_vec()))
            .collect();
        let header = header.map(|(name, value)| (name.to_ascii_lowercase(), value.to_owned()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let served = Arc::new(AtomicUsize::new(0));
        let (seen, count) = (requests.clone(), served.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                answer(stream, &files, header.as_ref(), &seen);
                count.fetch_add(1, Ordering::SeqCst);
            }
        });
        WebServer {
            address,
            requests,
            served,
        }
    }

    /// How many requests the server answered.
    pub fn served(&self) -> usize {
        self.served.load(Ordering::SeqCst)
    }
}

fn answer(
    mut stream: TcpStream,
    files: &HashMap<String, Vec<u8>>,
    header: Option<&(String, String)>,
    seen: &Mutex<Vec<String>>,
) {
    let mut reader = BufReader::new(stream.try_clone().expect("a stream"));
    let mut first = String::new();
    if reader.read_line(&mut first).is_err() {
        return;
    }
    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.trim().split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let mut parts = first.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    seen.lock()
        .expect("the list of requests")
        .push(format!("{method} {path}"));
    let allowed = header.is_none_or(|(name, value)| headers.get(name) == Some(value));
    let (status, body) = match files.get(path) {
        _ if !allowed => ("401 Unauthorized", b"no".to_vec()),
        Some(body) => match headers.get("range").and_then(|range| {
            range
                .strip_prefix("bytes=")?
                .strip_suffix('-')?
                .parse::<usize>()
                .ok()
        }) {
            Some(start) if start <= body.len() => ("206 Partial Content", body[start..].to_vec()),
            _ => ("200 OK", body.clone()),
        },
        None => ("404 Not Found", b"missing".to_vec()),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    if method != "HEAD" {
        let _ = stream.write_all(&body);
    }
}
