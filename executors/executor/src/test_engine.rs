//! Serves controlled Docker API replies for container lifecycle regressions.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{BufRead, BufReader, Read, Write};

use crate::ContainerEngine;

pub(crate) struct Request {
    pub method: String,
    pub path: String,
    pub body: serde_json::Value,
}

pub(crate) enum Reply {
    Json(u16, serde_json::Value),
    Lost,
    Hold,
}

pub(crate) struct Server {
    pub engine: ContainerEngine,
    thread: std::thread::JoinHandle<()>,
    _folder: tempfile::TempDir,
}

impl Server {
    pub(crate) async fn start(
        count: usize,
        mut reply: impl FnMut(Request) -> Reply + Send + 'static,
    ) -> Self {
        let folder = tempfile::tempdir().unwrap();
        let socket = folder.path().join("engine.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let thread = std::thread::spawn(move || {
            let mut held = Vec::new();
            let mut requests = 0;
            let mut identified = false;
            loop {
                let mut stream = listener.accept().unwrap().0;
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let mut parts = first.split_whitespace();
                let request = Request {
                    method: parts.next().unwrap().into(),
                    path: parts.next().unwrap().into(),
                    body: if body.is_empty() {
                        serde_json::Value::Null
                    } else {
                        serde_json::from_slice(&body).unwrap()
                    },
                };
                let answer = if request.path.ends_with("/version")
                    || request.path.ends_with("/info")
                {
                    identified |= request.path.ends_with("/info");
                    Reply::Json(
                        200,
                        serde_json::json!({"ID":"test-engine", "NCPU": 2, "MemTotal": 1073741824}),
                    )
                } else {
                    requests += 1;
                    reply(request)
                };
                let (status, body) = match answer {
                    Reply::Json(status, body) => (
                        status,
                        if status == 204 {
                            String::new()
                        } else {
                            body.to_string()
                        },
                    ),
                    Reply::Lost => {
                        if identified && requests == count {
                            break;
                        }
                        continue;
                    }
                    Reply::Hold => {
                        held.push(stream);
                        if identified && requests == count {
                            break;
                        }
                        continue;
                    }
                };
                write!(stream, "HTTP/1.1 {status} Reply\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                if identified && requests == count {
                    break;
                }
            }
        });
        let engine = ContainerEngine::connect(socket.to_str().unwrap())
            .await
            .unwrap()
            .with_journal_root(folder.path().join("journal"));
        Self {
            engine,
            thread,
            _folder: folder,
        }
    }

    pub(crate) fn finish(self) {
        self.thread.join().unwrap();
    }
}
