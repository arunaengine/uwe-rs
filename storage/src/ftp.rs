//! Files on FTP servers, for inputs: whether they exist, the entries of a directory, and
//! downloads that continue where a stopped one ended. Each request opens its own session in
//! passive mode, as FTP transfers one file per session at a time; the address of each server
//! is looked up once per process.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use suppaftp::FtpError;
use suppaftp::list::File;
use suppaftp::tokio::AsyncFtpStream;
use suppaftp::types::FileType;

use crate::error::Problem;
use crate::web::open;
use crate::{Entry, Metadata, address};

/// How long iunctus waits for a server to answer a connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) struct Ftp {
    user: String,
    password: String,
    /// The address of each server, by its name and port.
    servers: Mutex<HashMap<(String, u16), SocketAddr>>,
}

/// The server, the port and the path of an `ftp://` address.
struct Parts {
    host: String,
    port: u16,
    path: String,
}

impl Ftp {
    /// A client that logs in as `user`, or as `anonymous` without one, as public servers ask.
    pub fn new(user: Option<String>, password: Option<String>) -> Ftp {
        Ftp {
            user: user.unwrap_or_else(|| "anonymous".to_owned()),
            password: password.unwrap_or_else(|| "anonymous@".to_owned()),
            servers: Mutex::new(HashMap::new()),
        }
    }

    /// A session with the server of an address, logged in, for binary transfers.
    async fn session(&self, parts: &Parts) -> Result<AsyncFtpStream, Problem> {
        let key = (parts.host.clone(), parts.port);
        let known = self
            .servers
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(&key)
            .copied();
        let server = match known {
            Some(server) => server,
            None => {
                let mut found = tokio::net::lookup_host((parts.host.as_str(), parts.port))
                    .await
                    .map_err(|error| {
                        Problem::passing(format!("cannot find the server {}: {error}", parts.host))
                    })?;
                let server = found.next().ok_or_else(|| {
                    Problem::fixed(format!("the server {} has no address", parts.host))
                })?;
                (self.servers.lock())
                    .unwrap_or_else(|poison| poison.into_inner())
                    .insert(key, server);
                server
            }
        };
        let mut session = AsyncFtpStream::connect_timeout(server, CONNECT_TIMEOUT)
            .await
            .map_err(problem)?;
        session
            .login(&self.user, &self.password)
            .await
            .map_err(problem)?;
        session
            .transfer_type(FileType::Binary)
            .await
            .map_err(problem)?;
        Ok(session)
    }

    pub async fn stat(&self, target: &str) -> Result<Option<Metadata>, Problem> {
        let parts = parts(target)?;
        let mut session = self.session(&parts).await?;
        let found = match session.size(&parts.path).await {
            Ok(size) => Some(Metadata {
                directory: false,
                size: size as u64,
            }),
            // A directory has no size, and the server can change into it.
            Err(_) => session.cwd(&parts.path).await.ok().map(|()| Metadata {
                directory: true,
                size: 0,
            }),
        };
        let _ = session.quit().await;
        Ok(found)
    }

    pub async fn list(&self, target: &str) -> Result<Vec<Entry>, Problem> {
        let parts = parts(target)?;
        let mut session = self.session(&parts).await?;
        let lines = session.list(Some(&parts.path)).await.map_err(problem)?;
        let _ = session.quit().await;
        let mut entries: Vec<Entry> = lines
            .iter()
            .filter_map(|line| File::try_from(line.as_str()).ok())
            .filter(|file| !matches!(file.name(), "." | ".."))
            .map(|file| Entry {
                name: file.name().to_owned(),
                directory: file.is_directory(),
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    /// Downloads a file into `target`, from `offset` on if the server can.
    pub async fn download(&self, target: &str, file: &Path, offset: u64) -> Result<bool, Problem> {
        let parts = parts(target)?;
        let mut session = self.session(&parts).await?;
        let resumed = match usize::try_from(offset) {
            Ok(offset) if offset > 0 => session.resume_transfer(offset).await.is_ok(),
            _ => false,
        };
        let mut output = open(file, resumed).await?;
        let mut stream = session.retr_as_stream(&parts.path).await.map_err(problem)?;
        tokio::io::copy(&mut stream, &mut output)
            .await
            .map_err(|error| Problem::passing(error.to_string()))?;
        session
            .finalize_retr_stream(stream)
            .await
            .map_err(problem)?;
        let _ = session.quit().await;
        Ok(resumed)
    }
}

fn parts(target: &str) -> Result<Parts, Problem> {
    let rest = target
        .strip_prefix("ftp://")
        .ok_or_else(|| Problem::fixed("it is not an ftp:// address"))?;
    let (server, path) = rest.split_once('/').unwrap_or((rest, ""));
    // Credentials in the address are left to the provider's settings.
    let server = server.rsplit_once('@').map_or(server, |(_, server)| server);
    let (host, port) = match server.rsplit_once(':') {
        Some((host, port)) => (
            host,
            port.parse()
                .map_err(|_| Problem::fixed(format!("the port {port} is not a number")))?,
        ),
        None => (server, 21),
    };
    if host.is_empty() {
        return Err(Problem::fixed("it names no server"));
    }
    Ok(Parts {
        host: host.to_owned(),
        port,
        path: format!("/{}", address::decode(path)),
    })
}

fn problem(error: FtpError) -> Problem {
    match error {
        FtpError::ConnectionError(_) => Problem::passing(error.to_string()),
        _ => Problem::fixed(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_name_server_port_and_path() {
        let found = parts("ftp://ftp.ebi.ac.uk/pub/a%20b.txt").expect("an address");
        assert_eq!(
            (found.host.as_str(), found.port, found.path.as_str()),
            ("ftp.ebi.ac.uk", 21, "/pub/a b.txt")
        );
        let found = parts("ftp://user@127.0.0.1:2121/x").expect("an address");
        assert_eq!((found.host.as_str(), found.port), ("127.0.0.1", 2121));
        assert!(parts("ftp:///x").is_err());
    }
}
