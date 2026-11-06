//! Local SMTP and HTTP servers that record what uwe sends, and the reading of recorded mail.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// A local server that records each connection with `serve`, until `finish`.
pub struct Recorder {
    pub address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: JoinHandle<Vec<String>>,
}

impl Recorder {
    pub fn start(serve: fn(TcpStream) -> String) -> Recorder {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binds a free port");
        let address = listener.local_addr().expect("an address");
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut sessions = Vec::new();
            for stream in listener.incoming() {
                if stopping.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(120)));
                sessions.push(serve(stream));
            }
            sessions
        });
        Recorder {
            address,
            stop,
            thread,
        }
    }

    pub fn finish(self) -> Vec<String> {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        self.thread.join().expect("the recorder ends")
    }
}

/// An SMTP server that offers no AUTH and no STARTTLS, recorded like `fake_servers.py`:
/// `S:` and `C:` lines, and the message as sent.
pub fn smtp(stream: TcpStream) -> String {
    let mut log = String::new();
    let mut writer = stream.try_clone().expect("a second handle");
    let mut reader = BufReader::new(stream);
    let mut send = |log: &mut String, line: &str| {
        let _ = writer.write_all(format!("{line}\r\n").as_bytes());
        log.push_str(&format!("S: {line}\n"));
    };
    send(&mut log, "220 fake ESMTP");
    let mut data = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let line = line.replace("\r\n", "\n");
        if data {
            if line == ".\n" {
                data = false;
                send(&mut log, "250 OK queued");
            } else {
                log.push_str(&line);
            }
            continue;
        }
        log.push_str(&format!("C: {line}"));
        let command = line.trim().to_uppercase();
        if command.starts_with("EHLO") {
            let _ = reader.get_mut().write_all(b"250-fake\r\n250 8BITMIME\r\n");
        } else if command.starts_with("DATA") {
            send(&mut log, "354 go ahead");
            data = true;
        } else if command.starts_with("QUIT") {
            send(&mut log, "221 bye");
            break;
        } else {
            send(&mut log, "250 OK");
        }
    }
    log
}

/// An HTTP server that answers 500 for a path with `fail`, else 200, recorded as the request
/// line, the headers, an empty line and the body.
pub fn http(stream: TcpStream) -> String {
    let mut reader = BufReader::new(stream);
    let mut record = String::new();
    let mut length = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return record;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        // The request line without the protocol, as `fake_servers.py` recorded it.
        let line = line.strip_suffix(" HTTP/1.1").unwrap_or(line);
        if let Some((name, value)) = line.split_once(": ")
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.parse().unwrap_or(0);
        }
        record.push_str(&format!("{line}\n"));
    }
    let mut body = vec![0; length];
    let _ = reader.read_exact(&mut body);
    record.push('\n');
    record.push_str(&String::from_utf8_lossy(&body));
    let code = match record
        .lines()
        .next()
        .is_some_and(|line| line.contains("fail"))
    {
        true => "500 Internal Server Error",
        false => "200 OK",
    };
    let answer = format!("HTTP/1.1 {code}\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
    let _ = reader.get_mut().write_all(answer.as_bytes());
    record
}

/// Masks the run-specific values of these keys: up to a blank after a key that ends with `=`,
/// else up to `<` or the end of the line.
pub fn masked(text: &str) -> String {
    let keys = [
        "dateComplete=",
        "duration=",
        "Date Started: ",
        "Date Completed: ",
        "Pipeline script hash ID: ",
        "Nextflow Build: ",
        "Nextflow Compile Timestamp: ",
    ];
    let mut text = text.to_owned();
    for key in keys {
        let mut from = 0;
        while let Some(found) = text[from..].find(key) {
            let start = from + found + key.len();
            let ends: &[char] = match key.ends_with('=') {
                true => &[' ', '\n'],
                false => &['<', '\n'],
            };
            let end = text[start..]
                .find(ends)
                .map_or(text.len(), |end| start + end);
            text.replace_range(start..end, "*");
            from = start + 1;
        }
    }
    text
}

/// The parts of a recorded message, in order: the content type of each, and the body of each
/// part that is not multipart, with the `Content-ID` and the file name where there is one.
pub fn mail_parts(session: &str) -> (Vec<String>, Vec<String>) {
    let message = (session.lines())
        .skip_while(|line| !line.starts_with("S: 354"))
        .skip(1)
        .take_while(|line| !line.starts_with("S: 250"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut headers = Vec::new();
    let mut parts = Vec::new();
    walk(&message, true, &mut headers, &mut parts);
    (headers, parts)
}

fn walk(entity: &str, top: bool, headers: &mut Vec<String>, parts: &mut Vec<String>) {
    let (head, body) = entity.split_once("\n\n").unwrap_or((entity, ""));
    // Folded header lines continue the line before.
    let head = head.replace("\n\t", " ").replace("\n ", " ");
    let field = |name: &str| {
        (head.lines())
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case(name)
                    .then(|| value.trim().to_owned())
            })
            .unwrap_or_default()
    };
    if top {
        for name in ["From", "To", "Subject", "MIME-Version"] {
            headers.push(format!("{name}: {}", field(name)));
        }
    }
    let kind = field("Content-Type");
    let main = kind
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    if let Some(boundary) = kind.split("boundary=").nth(1) {
        let boundary = boundary.trim_matches('"');
        parts.push(main);
        let delimiter = format!("--{boundary}");
        for piece in body.split(&delimiter).skip(1) {
            if piece.starts_with("--") {
                break;
            }
            walk(piece.trim_start_matches('\n'), false, headers, parts);
        }
        return;
    }
    let disposition = field("Content-Disposition");
    let name = disposition
        .split("filename=")
        .nth(1)
        .unwrap_or_default()
        .trim_matches('"');
    let disposition = disposition.split(';').next().unwrap_or_default();
    let body = match field("Content-Transfer-Encoding").as_str() {
        "base64" => body.split_whitespace().collect::<String>(),
        "quoted-printable" => quoted(body.trim_end_matches('\n')),
        _ => body.trim_end_matches('\n').to_owned(),
    };
    parts.push(
        format!("{main} {disposition} {} {name}", field("Content-ID"))
            .trim()
            .to_owned(),
    );
    parts.push(masked(&body));
}

/// Quoted-printable text decoded: soft line breaks joined, `=XX` as its byte.
fn quoted(text: &str) -> String {
    let text = text.replace("=\n", "");
    let mut bytes = Vec::new();
    let mut rest = text.as_bytes();
    while let Some((&first, tail)) = rest.split_first() {
        let code = tail.get(..2).and_then(|hex| std::str::from_utf8(hex).ok());
        match code.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
            Some(byte) if first == b'=' => {
                bytes.push(byte);
                rest = &tail[2..];
            }
            _ => {
                bytes.push(first);
                rest = tail;
            }
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}
