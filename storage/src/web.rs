//! Reads web metadata and downloads files through a shared HTTP client.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{CONTENT_LENGTH, ETAG, HeaderMap, LAST_MODIFIED, RANGE};
use tokio::io::AsyncWriteExt;

use crate::error::Problem;
use crate::{Downloaded, Metadata, address};

/// A client for web servers.
#[derive(Clone)]
pub(crate) struct Web {
    client: reqwest::Client,
    /// The headers of the provider, which may hold secrets, sent with every request.
    headers: HeaderMap,
    /// Whether a refused request is an error. Public servers answer `HEAD` requests in many
    /// ways, so there any answer but success means that the file is not there, as before.
    strict: bool,
}

/// How long a request may make no progress: for a download no answer or read, for an upload
/// no data that the server acknowledges.
const IDLE: Duration = match cfg!(test) {
    true => Duration::from_secs(1),
    false => Duration::from_secs(300),
};

/// Shares one lazy HTTP client across storage and executors; bounded requests supply their own
/// timeouts.
pub fn http_client() -> Result<&'static reqwest::Client, String> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    let client = CLIENT
        .get_or_init(|| (builder()?.read_timeout(IDLE).build()).map_err(|error| error.to_string()));
    client.as_ref().map_err(Clone::clone)
}

/// The client for requests with a body. The read timeout of reqwest runs from the start of a
/// request to its answer, so it would end slow uploads; instead the kernel ends a connection
/// whose data stays unacknowledged, or unsent behind a closed window, for `IDLE`.
pub(crate) fn upload_client() -> Result<&'static reqwest::Client, String> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    let client = CLIENT.get_or_init(|| {
        let builder = builder()?.tcp_keepalive(Duration::from_secs(30));
        #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
        let builder = builder.tcp_user_timeout(IDLE);
        builder.build().map_err(|error| error.to_string())
    });
    client.as_ref().map_err(Clone::clone)
}

/// The client for requests with headers of a provider. It follows no redirect to another
/// origin, so that the request can go there again without those headers, which may hold
/// secrets; reqwest drops only `Authorization` and cookies on its own.
pub fn origin_client() -> Result<&'static reqwest::Client, String> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    let client = CLIENT.get_or_init(|| {
        let policy = reqwest::redirect::Policy::custom(|attempt| {
            let first = attempt.previous().first().map(origin);
            match attempt.previous().len() {
                10.. => attempt.error("too many redirects"),
                _ if first != Some(origin(attempt.url())) => attempt.stop(),
                _ => attempt.follow(),
            }
        });
        builder()?
            .read_timeout(IDLE)
            .redirect(policy)
            .build()
            .map_err(|error| error.to_string())
    });
    client.as_ref().map_err(Clone::clone)
}

/// The scheme, host and port of an address.
fn origin(url: &reqwest::Url) -> (String, Option<String>, Option<u16>) {
    let host = url.host_str().map(str::to_owned);
    (url.scheme().to_owned(), host, url.port_or_known_default())
}

/// The settings that the clients of uwe share.
fn builder() -> Result<reqwest::ClientBuilder, String> {
    let mut builder = reqwest::Client::builder()
        .user_agent(concat!("uwe/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
        // Files arrive as their bytes: a decoded body would not match the size and version
        // of its `HEAD` answer, and a `.gz` file served as `gzip` would arrive unpacked.
        .no_gzip()
        .no_brotli()
        .no_zstd()
        .no_deflate();
    if MOZILLA_ROOTS.get().is_some() {
        let roots = webpki_root_certs::TLS_SERVER_ROOT_CERTS
            .iter()
            .filter_map(|root| reqwest::Certificate::from_der(root.as_ref()).ok());
        builder = builder.tls_certs_merge(roots);
    }
    if let Some(address) = (PROXY.get())
        .and_then(Option::as_deref)
        .filter(|_| !proxy_in_environment())
    {
        let proxy = reqwest::Proxy::all(address)
            .map_err(|error| format!("the proxy {address} of the config file: {error}"))?;
        builder = builder.proxy(proxy.no_proxy(reqwest::NoProxy::from_env()));
    }
    Ok(builder)
}

/// The web proxy of `[network] proxy` of the config file, once the first request of the
/// process settled it; `None` inside sends requests directly.
static PROXY: OnceLock<Option<String>> = OnceLock::new();

/// Whether the client trusts the roots of Mozilla besides those of the system.
static MOZILLA_ROOTS: OnceLock<()> = OnceLock::new();

/// Trusts the roots of Mozilla besides those of the system, for the helper on a node whose
/// image may have no certificates of its own. Takes effect only before the first request.
pub fn trust_mozilla_roots() {
    let _ = MOZILLA_ROOTS.set(());
}

/// Sends the requests of uwe through this web proxy, or directly for `None`, unless the
/// environment names one, like `HTTPS_PROXY`, which wins. The process shares its clients, so
/// the first call settles the proxy and a later call with another one is refused.
pub fn use_proxy(address: Option<&str>) -> Result<(), String> {
    let settled = PROXY.get_or_init(|| address.map(str::to_owned));
    if settled.as_deref() == address {
        return Ok(());
    }
    // The address can hold a password, so the message does not name it.
    Err(match settled {
        Some(_) => "this process sends its web requests through another proxy than the config             file names; run requests with other proxy settings in another process"
            .to_owned(),
        None => "this process sends its web requests without the proxy that the config file             names; run requests with other proxy settings in another process"
            .to_owned(),
    })
}

/// The web proxy of the config file, unless the environment names one. Programs that uwe
/// starts on other machines, like the jobs of SLURM, need it to reach the web as uwe does.
pub fn config_proxy() -> Option<&'static str> {
    PROXY
        .get()
        .and_then(Option::as_deref)
        .filter(|_| !proxy_in_environment())
}

/// Whether the environment names a web proxy, which the client then takes as it is.
fn proxy_in_environment() -> bool {
    ["HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY"]
        .iter()
        .any(|name| {
            [name.to_string(), name.to_lowercase()]
                .iter()
                .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
        })
}

/// [`http_client`], as a clone (a handle to the same client) for code that keeps one.
pub(crate) fn shared_client() -> Result<reqwest::Client, Problem> {
    http_client().cloned().map_err(Problem::fixed)
}

impl Web {
    /// The client for public files.
    pub fn public() -> Result<Web, Problem> {
        Ok(Web {
            client: shared_client()?,
            headers: HeaderMap::new(),
            strict: false,
        })
    }

    /// A client that sends these headers with every request, which hold secrets.
    pub fn with_headers(headers: HeaderMap) -> Result<Web, Problem> {
        Ok(Web {
            client: origin_client().cloned().map_err(Problem::fixed)?,
            headers,
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
        let response = self.request(self.client.head(&url)).await?;
        let status = response.status();
        if status.is_success() {
            // The header gives the size of the file; the response itself has no body.
            let headers = response.headers();
            let size = (headers.get(CONTENT_LENGTH))
                .and_then(|length| length.to_str().ok()?.parse().ok())
                .unwrap_or(0);
            let header = |name| headers.get(name)?.to_str().ok().map(str::to_owned);
            return Ok(Some(Metadata {
                directory: false,
                modified: header(LAST_MODIFIED)
                    .and_then(|value| opendal::raw::Timestamp::parse_rfc2822(&value).ok())
                    .map(std::time::SystemTime::from),
                size,
                version: header(ETAG).or_else(|| header(LAST_MODIFIED)),
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
        let response = self.request(self.client.get(url)).await?;
        let status = response.status();
        Ok((status, response.text().await.map_err(network)?))
    }

    /// Downloads a file into `target`. With an `offset`, it asks for the rest of the file only
    /// and appends it, if the server can; else it writes the whole file. The version is the
    /// ETag of the answer, else the time of the last change it gives.
    pub async fn download(
        &self,
        url: &str,
        target: &Path,
        offset: u64,
    ) -> Result<Downloaded, Problem> {
        let mut request = self.client.get(self.address(url));
        if offset > 0 {
            request = request.header(RANGE, format!("bytes={offset}-"));
        }
        let mut response = self.request(request).await?;
        let status = response.status();
        if !status.is_success() {
            return Err(self.refused(status));
        }
        if offset == 0 && status == StatusCode::PARTIAL_CONTENT {
            return Err(Problem::fixed(
                "the server returned a partial response for a whole file",
            ));
        }
        let resumed = status == StatusCode::PARTIAL_CONTENT;
        let header = |name| {
            response
                .headers()
                .get(name)?
                .to_str()
                .ok()
                .map(str::to_owned)
        };
        let version = header(ETAG).or_else(|| header(LAST_MODIFIED));
        let mut file = open(target, resumed).await?;
        while let Some(chunk) = response.chunk().await.map_err(network)? {
            file.write_all(&chunk)
                .await
                .map_err(|error| Problem::local(target, error))?;
        }
        file.flush()
            .await
            .map_err(|error| Problem::local(target, error))?;
        Ok(Downloaded { version })
    }

    /// Sends a request with the headers of the provider. A redirect to another origin goes
    /// there again without them.
    async fn request(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, Problem> {
        let again = request.try_clone();
        let response = (request.headers(self.headers.clone()).send())
            .await
            .map_err(network)?;
        let location = (response.headers().get(reqwest::header::LOCATION))
            .and_then(|location| location.to_str().ok())
            .and_then(|location| response.url().join(location).ok());
        match (response.status().is_redirection(), location, again) {
            (true, Some(next), Some(again)) if !self.headers.is_empty() => {
                let mut request = again.build().map_err(network)?;
                *request.url_mut() = next;
                shared_client()?.execute(request).await.map_err(network)
            }
            _ => Ok(response),
        }
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

/// Opens an existing private download file; a cancelled open must never recreate its name.
pub(crate) async fn open(target: &Path, append: bool) -> Result<tokio::fs::File, Problem> {
    tokio::fs::OpenOptions::new()
        .create(false)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(target)
        .await
        .map_err(|error| Problem::local(target, error))
}

/// A request that did not reach the server or broke off, which another try may fix.
pub(crate) fn network(error: reqwest::Error) -> Problem {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares_process_client() {
        let first = http_client().expect("a client");
        let second = http_client().expect("a client");
        assert!(std::ptr::eq(first, second));
    }

    #[test]
    fn cancellation_cleans_paths() {
        use std::future::Future;
        for operation in 0..3 {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .max_blocking_threads(1)
                .enable_all()
                .build()
                .unwrap();
            let root = tempfile::tempdir().unwrap();
            let inputs = tempfile::tempdir().unwrap();
            let source = inputs.path().join("source");
            std::fs::write(&source, b"new").unwrap();
            let target = root.path().join("old");
            std::fs::write(&target, b"old").unwrap();
            let storage = crate::Storage::default();
            storage.pin(crate::Fetched {
                address: "https://test/source".into(),
                copy: source.clone(),
                digest: "unused".into(),
                size: 3,
                time: std::time::UNIX_EPOCH,
                version: None,
            });
            let (ready, started) = std::sync::mpsc::channel();
            let (release, blocked) = std::sync::mpsc::channel();
            runtime.spawn_blocking(move || {
                ready.send(()).unwrap();
                blocked.recv().unwrap();
            });
            started.recv().unwrap();
            runtime.block_on(async {
                let mut temporary = Some(tempfile::NamedTempFile::new_in(root.path()).unwrap());
                let name = temporary.as_ref().unwrap().path().to_owned();
                let mut pending: std::pin::Pin<Box<dyn Future<Output = ()> + '_>> = match operation
                {
                    0 => Box::pin(async {
                        let _ = open(&name, false).await;
                    }),
                    1 => Box::pin(async {
                        let _ = storage.copy_of("https://test/source", &target).await;
                    }),
                    _ => {
                        let part = temporary.take().unwrap();
                        Box::pin(async {
                            let _ = crate::folder::copied(&source, part).await;
                        })
                    }
                };
                let mut context = std::task::Context::from_waker(std::task::Waker::noop());
                assert!(pending.as_mut().poll(&mut context).is_pending());
                assert_eq!(
                    std::fs::read_dir(root.path()).unwrap().count(),
                    if operation == 1 { 3 } else { 2 }
                );
                drop(pending);
                drop(temporary);
            });
            release.send(()).unwrap();
            runtime.block_on(async {
                tokio::task::spawn_blocking(|| ()).await.unwrap();
            });
            assert_eq!(std::fs::read(target).unwrap(), b"old");
            assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
        }
    }
}
