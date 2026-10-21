//! A small FTP server for tests: one user, files from memory, passive mode, and the commands
//! that uwe sends: USER, PASS, TYPE, SIZE, CWD, PASV, REST, RETR, LIST and QUIT.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

pub struct FtpServer {
    /// The address of the server, like `ftp://127.0.0.1:4711`.
    pub address: String,
}

impl FtpServer {
    /// Serves `files` by path, like `/pub/a.txt`, to `user` with `password`.
    pub fn start(files: &[(&str, &str)], user: &str, password: &str) -> FtpServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
        let address = format!("ftp://{}", listener.local_addr().expect("an address"));
        let files: BTreeMap<String, Vec<u8>> = files
            .iter()
            .map(|(path, body)| (path.to_string(), body.as_bytes().to_vec()))
            .collect();
        let login = (user.to_owned(), password.to_owned());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (files, login) = (files.clone(), login.clone());
                std::thread::spawn(move || session(stream, &files, &login));
            }
        });
        FtpServer { address }
    }
}

fn session(stream: TcpStream, files: &BTreeMap<String, Vec<u8>>, login: &(String, String)) {
    let mut reader = BufReader::new(stream.try_clone().expect("a stream"));
    let mut writer = stream;
    let mut say = |text: &str| writer.write_all(format!("{text}\r\n").as_bytes());
    let _ = say("220 uwe test server");
    let (mut user, mut data, mut offset) = (String::new(), None::<TcpListener>, 0);
    let is_directory = |path: &str| {
        let folder = format!("{}/", path.trim_end_matches('/'));
        files.keys().any(|file| file.starts_with(&folder))
    };
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let line = line.trim_end();
        let (command, argument) = line.split_once(' ').unwrap_or((line, ""));
        let answer = match command {
            "USER" => {
                user = argument.to_owned();
                "331 password please".to_owned()
            }
            "PASS" if user == login.0 && argument == login.1 => "230 welcome".to_owned(),
            "PASS" => "530 wrong user or password".to_owned(),
            "TYPE" => "200 type set".to_owned(),
            "SIZE" => match files.get(argument) {
                Some(body) => format!("213 {}", body.len()),
                None => "550 no such file".to_owned(),
            },
            "CWD" if is_directory(argument) => "250 changed".to_owned(),
            "CWD" => "550 no such folder".to_owned(),
            "PASV" => {
                let listener = TcpListener::bind("127.0.0.1:0").expect("a data port");
                let port = listener.local_addr().expect("an address").port();
                data = Some(listener);
                format!("227 passive (127,0,0,1,{},{})", port / 256, port % 256)
            }
            "REST" => {
                offset = argument.parse().unwrap_or(0);
                "350 restarting".to_owned()
            }
            "RETR" | "LIST" => {
                let body = match command {
                    "RETR" => files
                        .get(argument)
                        .map(|body| body[offset.min(body.len())..].to_vec()),
                    _ => Some(listing(files, argument)),
                };
                offset = 0;
                match (body, data.take()) {
                    (Some(body), Some(listener)) => {
                        let _ = say("150 sending");
                        if let Ok((mut connection, _)) = listener.accept() {
                            let _ = connection.write_all(&body);
                        }
                        "226 sent".to_owned()
                    }
                    _ => "550 no such file".to_owned(),
                }
            }
            "QUIT" => {
                let _ = say("221 bye");
                return;
            }
            _ => "502 not here".to_owned(),
        };
        if say(&answer).is_err() {
            return;
        }
    }
}

/// The entries of a folder as `ls -l` lines.
fn listing(files: &BTreeMap<String, Vec<u8>>, folder: &str) -> Vec<u8> {
    let folder = format!("{}/", folder.trim_end_matches('/'));
    let mut entries = BTreeMap::new();
    for (path, body) in files {
        let Some(rest) = path.strip_prefix(&folder) else {
            continue;
        };
        match rest.split_once('/') {
            Some((directory, _)) => entries.insert(directory.to_owned(), None),
            None => entries.insert(rest.to_owned(), Some(body.len())),
        };
    }
    let mut text = String::new();
    for (name, size) in entries {
        let line = match size {
            Some(size) => format!("-rw-r--r-- 1 owner group {size} Jan 01 00:00 {name}\r\n"),
            None => format!("drwxr-xr-x 1 owner group 0 Jan 01 00:00 {name}\r\n"),
        };
        text.push_str(&line);
    }
    text.into_bytes()
}
