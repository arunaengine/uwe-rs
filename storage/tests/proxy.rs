//! Checks that each storage sends its web requests through its own proxy within one process.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// A proxy that answers every request itself with four bytes, and keeps each request line.
fn proxy() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let kept = seen.clone();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|count| count > 2) {
                line.clear();
            }
            kept.lock().unwrap().push(first.trim().to_owned());
            let answer = "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nETag: \"v1\"\r\n\
                          Connection: close\r\n\r\n";
            let _ = stream.write_all(answer.as_bytes());
        }
    });
    (address, seen)
}

// The process proxy is shared by the whole test binary, so this file holds a single test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxies_per_storage() {
    let names = ["HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY"];
    let named = names
        .iter()
        .flat_map(|name| [name.to_string(), name.to_lowercase()]);
    if named
        .into_iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
    {
        eprintln!("the environment names a web proxy, which wins over those of the storages");
        return;
    }
    let (first, first_seen) = proxy();
    let (second, second_seen) = proxy();
    storage::use_proxy(Some(&first)).expect("the first proxy");
    let other = "http://user:secret-word@other.example.org:3128";
    storage::use_proxy(Some(other)).expect("a later proxy is no longer refused");
    storage::use_proxy(None).expect("no proxy is no longer refused");
    assert_eq!(storage::config_proxy(), Some(first.as_str()));
    let base = storage::Storage::default();
    assert_eq!(base.proxy(), Some(first.as_str()), "the process proxy");
    let own = base.with_proxy(Some(&second));
    assert_eq!(own.proxy(), Some(second.as_str()));
    assert_eq!(base.with_proxy(None).proxy(), None);
    let url = "http://files.example.invalid/data.txt";
    let session = own.session();
    let (through_first, through_second) = tokio::join!(base.stat(url), session.stat(url));
    assert_eq!(through_first.unwrap().map(|found| found.size), Some(4));
    assert_eq!(through_second.unwrap().map(|found| found.size), Some(4));
    let line = format!("HEAD {url} HTTP/1.1");
    assert_eq!(*first_seen.lock().unwrap(), std::slice::from_ref(&line));
    assert_eq!(*second_seen.lock().unwrap(), [line]);
}
