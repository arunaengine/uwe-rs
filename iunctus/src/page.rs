//! The web page of runs (decision 23): `iunctus page [<run>]` serves it for any run, and
//! `iunctus run -page` serves it for the run it starts. The page listens on this machine only,
//! unless `-address` says otherwise.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::IsTerminal;
use std::net::{IpAddr, Ipv4Addr};
use std::process::{Command, Stdio};

use run_page::{Server, Settings};

use crate::arguments::Arguments;
use crate::{Failure, failure, runs};

/// Starts the page and says where it is; `path` is the part of the address after the host,
/// like `/runs/<run>`. In a terminal, the browser opens it too.
pub fn start(arguments: &Arguments, path: &str) -> Result<Server, Failure> {
    let settings = Settings {
        database: runs::database_path(arguments),
        address: arguments.address.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        port: arguments.port.unwrap_or(0),
    };
    let server = Server::start(&settings).map_err(|error| failure(format!("iunctus: {error}")))?;
    let url = format!("{}{path}", server.url());
    eprintln!("iunctus: the page is at {url}");
    if !settings.address.is_loopback() {
        eprintln!(
            "iunctus: the page listens on {}, so everyone who can reach this machine sees the \
             runs, their logs and their files",
            settings.address
        );
    }
    if std::io::stderr().is_terminal() {
        open(&url);
    }
    Ok(server)
}

/// `iunctus page [<run>]`: serves the page until Ctrl-C.
pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    let path = match &arguments.target {
        Some(run) => format!("/runs/{}", run.to_string_lossy()),
        None => "/".to_owned(),
    };
    let server = start(arguments, &path)?;
    eprintln!("iunctus: press Ctrl-C to stop the page");
    server.serve_until_interrupted();
    Ok(())
}

/// Opens an address in the browser of the desktop, if there is one; nothing when it fails.
fn open(url: &str) {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = Command::new(program)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}
