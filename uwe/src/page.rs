//! Serves recorded run pages on the configured local address.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{IsTerminal, Read};
use std::net::{IpAddr, Ipv4Addr};
use std::process::{Command, Stdio};

use std::sync::Arc;

use run_page::{Server, Settings, Studio};

use crate::arguments::Arguments;
use crate::{Failure, runs};

/// Starts the page and says where it is; `path` is the part of the address after the host,
/// like `/runs/<run>`. In a terminal, the browser opens it too.
pub fn start(arguments: &Arguments, path: &str) -> Result<Server, Failure> {
    serve(arguments, path, None, true)
}

/// Starts the page, with the launcher and the combiner of `studio` when there is one. The
/// browser opens it with `--open`, and in a terminal when `browse` says so.
pub fn serve(
    arguments: &Arguments,
    path: &str,
    studio: Option<Arc<dyn Studio>>,
    browse: bool,
) -> Result<Server, Failure> {
    let config = config_file::ConfigFile::load(arguments.config_file.as_deref())
        .map_err(|error| Failure::setup(format!("uwe: {error}")))?
        .with_secrets(arguments.secrets.clone());
    let token = (|| {
        let Some(page) = config.section("page")? else {
            return Ok(None);
        };
        page.only(&["token"])?;
        page.secret("token")?
            .map(|secret| secret.reveal())
            .transpose()
    })()
    .map_err(|error: config_file::ConfigError| Failure::setup(format!("uwe: {error}")))?;
    let mut settings = Settings {
        database: runs::database_path(arguments),
        address: arguments.address.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        port: arguments.port.unwrap_or(0),
        token: arguments.token.clone().or(token),
    };
    // A token that the user chose stays out of the terminal and its logs; a fresh one is shown.
    let chosen = settings.token.is_some();
    if !settings.address.is_loopback() && settings.token.is_none() {
        let mut bytes = [0_u8; 32];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut random| random.read_exact(&mut bytes))
            .map_err(|error| {
                Failure::setup(format!("uwe: cannot generate a page token: {error}"))
            })?;
        settings.token = Some(bytes.iter().map(|byte| format!("{byte:02x}")).collect());
    }
    let store = arguments.run_store.as_ref().map(|host| host.0.clone());
    let server = match (studio, store) {
        (Some(studio), store) => Server::start_studio(&settings, store, studio, None),
        (None, Some(store)) => Server::start_store(&settings, store, None),
        (None, None) => Server::start(&settings),
    }
    .map_err(|error| Failure::setup(format!("uwe: {error}")))?;
    let base = format!("{}{path}", server.url());
    let url = match &settings.token {
        Some(token) => format!("{base}?token={token}"),
        None => base.clone(),
    };
    if chosen {
        eprintln!("uwe: the page is at {base}; open it with `?token=` and the configured token");
    } else {
        eprintln!("uwe: the page is at {url}");
    }
    if !settings.address.is_loopback() && !chosen {
        eprintln!(
            "uwe: the page listens on {} and requires the token in this URL",
            settings.address
        );
    }
    if arguments.open || (browse && std::io::stderr().is_terminal()) {
        open(&url);
    }
    Ok(server)
}

/// `uwe page [<run>]`: serves the page until Ctrl-C. With `--tools` or `--workflows` it serves
/// the launcher and the combiner too, as `uwe serve` does.
pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    let path = match &arguments.target {
        Some(run) => format!("/runs/{}", run.to_string_lossy()),
        None => "/".to_owned(),
    };
    let interactive = !arguments.tools.is_empty() || !arguments.workflows.is_empty();
    let backend = match interactive {
        true => Some(Arc::new(crate::serve::Backend::new(crate::serve::options(
            arguments, false,
        )?))),
        false => None,
    };
    let studio = backend.clone().map(|backend| backend as Arc<dyn Studio>);
    let server = serve(arguments, &path, studio, true)?;
    eprintln!("uwe: press Ctrl-C to stop the page");
    server.serve_until_interrupted();
    if let Some(backend) = backend {
        backend.stop();
    }
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
