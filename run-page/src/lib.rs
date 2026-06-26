//! The web page of iunctus (decision 23). The binary serves it on localhost; it reads the run
//! database and shows every run: the graph as it grows, the state of each step and task, the
//! standard output and error of each task, and the outputs with links to download them. A
//! running run updates the page live through Server-Sent Events. The page is plain HTML, CSS
//! and JavaScript inside the binary: nothing to install and no build step.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod api;

use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use run_database::{Database, DatabaseError};
use tokio::sync::oneshot;

/// Where the page serves and what it reads.
#[derive(Clone, Debug)]
pub struct Settings {
    /// The run database file.
    pub database: PathBuf,
    /// The address to listen on; only this machine by default.
    pub address: IpAddr,
    /// The port; 0 lets the system choose a free one.
    pub port: u16,
}

impl Settings {
    pub fn new(database: PathBuf) -> Self {
        Settings {
            database,
            address: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 0,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PageError {
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("the web page cannot listen on {address}: {error}")]
    Listen {
        address: SocketAddr,
        error: std::io::Error,
    },
    #[error("the web page cannot start: {0}")]
    Start(std::io::Error),
}

/// A running page. It serves on a thread of its own until it is stopped.
pub struct Server {
    url: String,
    runtime: tokio::runtime::Handle,
    stop: oneshot::Sender<()>,
    thread: std::thread::JoinHandle<()>,
}

impl Server {
    /// Opens the database and starts serving. The address is bound before this returns, so
    /// the page can be opened at once.
    pub fn start(settings: &Settings) -> Result<Server, PageError> {
        let database = Database::open(&settings.database)?;
        let address = SocketAddr::new(settings.address, settings.port);
        let listener =
            TcpListener::bind(address).map_err(|error| PageError::Listen { address, error })?;
        let bound = listener.local_addr().map_err(PageError::Start)?;
        listener.set_nonblocking(true).map_err(PageError::Start)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("iunctus-page")
            .enable_all()
            .build()
            .map_err(PageError::Start)?;
        let handle = runtime.handle().clone();
        let state = Arc::new(State {
            database: Mutex::new(database),
        });
        let (stop, stopped) = oneshot::channel();
        let thread = std::thread::Builder::new()
            .name("iunctus-page".into())
            .spawn(move || {
                runtime.block_on(async move {
                    let Ok(listener) = tokio::net::TcpListener::from_std(listener) else {
                        return;
                    };
                    let router = api::router(state);
                    let shutdown = async {
                        let _ = stopped.await;
                    };
                    let _ = axum::serve(listener, router)
                        .with_graceful_shutdown(shutdown)
                        .await;
                });
            })
            .map_err(PageError::Start)?;
        let host = match bound.ip() {
            IpAddr::V6(ip) => format!("[{ip}]"),
            IpAddr::V4(ip) => ip.to_string(),
        };
        Ok(Server {
            url: format!("http://{host}:{}", bound.port()),
            runtime: handle,
            stop,
            thread,
        })
    }

    /// The address of the page, like `http://127.0.0.1:43127`.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Serves until the user presses Ctrl-C, then stops. Ctrl-C is caught only from now on, so
    /// that it still stops a run that goes on before.
    pub fn serve_until_interrupted(self) {
        let _ = self.runtime.block_on(tokio::signal::ctrl_c());
        self.stop();
    }

    /// Stops serving and waits for the open requests to end.
    pub fn stop(self) {
        let _ = self.stop.send(());
        let _ = self.thread.join();
    }
}

/// What every request shares: one connection to the database.
pub(crate) struct State {
    database: Mutex<Database>,
}

pub(crate) type Shared = Arc<State>;

/// Runs a query of the database on a thread for blocking work.
pub(crate) async fn query<T: Send + 'static>(
    state: &Shared,
    work: impl FnOnce(&Database) -> Result<T, Failure> + Send + 'static,
) -> Result<T, Failure> {
    let state = state.clone();
    tokio::task::spawn_blocking(move || {
        let database = state
            .database
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        work(&database)
    })
    .await
    .map_err(|error| Failure::internal(error.to_string()))?
}

/// Why a request failed, as an HTTP status and a plain message.
#[derive(Debug)]
pub(crate) struct Failure {
    status: StatusCode,
    message: String,
}

impl Failure {
    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Failure {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Failure {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Failure {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

impl From<DatabaseError> for Failure {
    fn from(error: DatabaseError) -> Self {
        Failure::internal(error.to_string())
    }
}

impl From<std::io::Error> for Failure {
    fn from(error: std::io::Error) -> Self {
        Failure::internal(error.to_string())
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (self.status, self.message).into_response()
    }
}
