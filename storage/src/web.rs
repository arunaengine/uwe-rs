//! Files on web servers: whether they exist, and downloads that continue where a stopped one
//! ended. All requests without headers of their own share one client per process, so that
//! connections are reused and a host's address is looked up once; a provider with headers,
//! like a token, has one client of its own.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{CONTENT_LENGTH, HeaderMap, RANGE};
use tokio::io::AsyncWriteExt;

use crate::error::Problem;
use crate::{Metadata, address};

/// A client for web servers.
#[derive(Clone)]
pub(crate) struct Web {
    client: reqwest::Client,
    /// Whether a refused request is an error. Public servers answer `HEAD` requests in many
    /// ways, so there any answer but success means that the file is not there, as before.
    strict: bool,
}

/// The settings that every client shares: its name, and time limits that allow long downloads.
fn builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .user_agent(concat!("iunctus/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(300))
}

/// The client of every request without headers of its own, made once per process.
pub(crate) fn shared_client() -> Result<reqwest::Client, Problem> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    let client = CLIENT.get_or_init(|| builder().build().map_err(|error| error.to_string()));
    client.clone().map_err(Problem::fixed)
}

impl Web {
    /// The client for public files.
    pub fn public() -> Result<Web, Problem> {
        Ok(Web {
            client: shared_client()?,
            strict: false,
        })
    }

    /// A client that sends these headers with every request, which hold secrets.
    pub fn with_headers(headers: HeaderMap) -> Result<Web, Problem> {
        let client = builder()
            .default_headers(headers)
            .build()
            .map_err(|error| Problem::fixed(error.to_string()))?;
        Ok(Web {
            client,
            strict: true,
        })
    }

    /// Where a request for `url` goes: a public file of a GitHub repository goes straight to
    /// the host of raw files, which saves a redirect.
    fn address(&self, url: &str) -> String {
        match self.strict {
            true => url.to_owned(),
            false => address::raw_github(url).unwrap_or_else(|| url.to_owned()),
        }
    }

    /// Whether a file exists, and its size, from a `HEAD` request.
    pub async fn stat(&self, url: &str) -> Result<Option<Metadata>, Problem> {
        let url = self.address(url);
        let response = self.client.head(&url).send().await.map_err(network)?;
        let status = response.status();
        if status.is_success() {
            // The header gives the size of the file; the response itself has no body.
            let size = response
                .headers()
                .get(CONTENT_LENGTH)
                .and_then(|length| length.to_str().ok()?.parse().ok())
                .unwrap_or(0);
            return Ok(Some(Metadata {
                directory: false,
                size,
            }));
        }
        match status {
            StatusCode::NOT_FOUND | StatusCode::GONE => Ok(None),
            status if self.strict => Err(self.refused(status)),
            _ => Ok(None),
        }
    }

    /// The text at an address, like a listing of a bucket.
    pub async fn text(&self, url: &str) -> Result<String, Problem> {
        let (status, text) = self.answer(url).await?;
        match status.is_success() {
            true => Ok(text),
            false => Err(self.refused(status)),
        }
    }

    /// The status and the text of the answer to a `GET` request, whatever the status.
    pub async fn answer(&self, url: &str) -> Result<(StatusCode, String), Problem> {
        let response = self.client.get(url).send().await.map_err(network)?;
        let status = response.status();
        Ok((status, response.text().await.map_err(network)?))
    }

    /// Downloads a file into `target`. With an `offset`, it asks for the rest of the file only
    /// and appends it, if the server can; else it writes the whole file. Returns whether it
    /// appended.
    pub async fn download(&self, url: &str, target: &Path, offset: u64) -> Result<bool, Problem> {
        let mut request = self.client.get(self.address(url));
        if offset > 0 {
            request = request.header(RANGE, format!("bytes={offset}-"));
        }
        let mut response = request.send().await.map_err(network)?;
        let status = response.status();
        if !status.is_success() {
            return Err(self.refused(status));
        }
        let resumed = status == StatusCode::PARTIAL_CONTENT;
        let mut file = open(target, resumed).await?;
        while let Some(chunk) = response.chunk().await.map_err(network)? {
            file.write_all(&chunk)
                .await
                .map_err(|error| Problem::local(target, error))?;
        }
        file.flush()
            .await
            .map_err(|error| Problem::local(target, error))?;
        Ok(resumed)
    }

    /// An answer that is not a success; errors of the server may go away.
    fn refused(&self, status: StatusCode) -> Problem {
        let message = match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN if self.strict => format!(
                "the server refused access ({status}); check the credentials of the provider"
            ),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                format!("the server refused access ({status})")
            }
            StatusCode::NOT_FOUND | StatusCode::GONE => return Problem::missing(),
            _ => format!("the server answered {status}"),
        };
        match status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
            true => Problem::passing(message),
            false => Problem::fixed(message),
        }
    }
}

/// Opens the file that a download writes: to append to, or to write anew.
pub(crate) async fn open(target: &Path, append: bool) -> Result<tokio::fs::File, Problem> {
    tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(target)
        .await
        .map_err(|error| Problem::local(target, error))
}

/// A request that did not reach the server or broke off, which another try may fix.
fn network(error: reqwest::Error) -> Problem {
    // The message of reqwest names the address, which the caller names already, and the
    // cause of the error comes after it.
    let error = error.without_url();
    let mut message = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        message = format!("{message}: {cause}");
        source = cause.source();
    }
    Problem::passing(message)
}
