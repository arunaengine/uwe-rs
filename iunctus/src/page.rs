//! Serves recorded run pages on the configured local address.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{IsTerminal, Read};
use std::net::{IpAddr, Ipv4Addr};
use std::process::{Command, Stdio};

use run_page::{Server, Settings};

use crate::arguments::Arguments;
use crate::{Failure, runs};

/// Starts the page and says where it is; `path` is the part of the address after the host,
/// like `/runs/<run>`. In a terminal, the browser opens it too.
pub fn start(arguments: &Arguments, path: &str) -> Result<Server, Failure> {
    let config = config_file::ConfigFile::load(arguments.config_file.as_deref())
        .map_err(|error| Failure::setup(format!("iunctus: {error}")))?;
    let token = (|| {
        let Some(page) = config.section("page")? else {
            return Ok(None);
        };
        page.only(&["token"])?;
        page.secret("token")?
            .map(|secret| secret.reveal())
            .transpose()
    })()
    .map_err(|error: config_file::ConfigError| Failure::setup(format!("iunctus: {error}")))?;
    let mut settings = Settings {
        database: runs::database_path(arguments),
        address: arguments.address.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        port: arguments.port.unwrap_or(0),
        token,
    };
    if !settings.address.is_loopback() && settings.token.is_none() {
        let mut bytes = [0_u8; 32];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut random| random.read_exact(&mut bytes))
            .map_err(|error| {
                Failure::setup(format!("iunctus: cannot generate a page token: {error}"))
            })?;
        settings.token = Some(bytes.iter().map(|byte| format!("{byte:02x}")).collect());
    }
    let server =
        Server::start(&settings).map_err(|error| Failure::setup(format!("iunctus: {error}")))?;
    let mut url = format!("{}{path}", server.url());
    if let Some(token) = &settings.token {
        url.push_str(&format!("?token={token}"));
    }
    eprintln!("iunctus: the page is at {url}");
    if !settings.address.is_loopback() {
        eprintln!(
            "iunctus: the page listens on {} and requires the token in this URL",
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
