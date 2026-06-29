//! The interface between the engine and the executors that run tasks: the simulator, local
//! containers, TES and clusters. An executor starts an attempt of a task and gives back a
//! future of its outcome, which the engine drives on Tokio while the driver waits on its own
//! thread.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};

pub use tokio_util::sync::CancellationToken;

use crate::{ExecuteError, Outcome, Task};

/// One attempt of a task.
#[derive(Clone, Debug)]
pub struct Attempt {
    pub task: Arc<Task>,
    /// Counts from 1 for each task: a failure of the machine, or a retry that the driver asks
    /// for, starts the next attempt.
    pub number: u32,
    /// Fires when the run stops, so that the attempt stops too.
    pub cancel: CancellationToken,
}

/// An attempt that runs, and its outcome once it ends.
pub struct Running(State);

enum State {
    /// The attempt ended when it started; `None` once the outcome was taken.
    Ended(Option<Result<Outcome, ExecuteError>>),
    Future(Pin<Box<dyn Future<Output = Result<Outcome, ExecuteError>> + Send>>),
}

impl Running {
    /// An attempt that ends when the future does.
    pub fn new(
        future: impl Future<Output = Result<Outcome, ExecuteError>> + Send + 'static,
    ) -> Self {
        Running(State::Future(Box::pin(future)))
    }

    /// An attempt that ended when it started, like one of the simulator, which does its work
    /// on the thread of the driver.
    pub fn ended(result: Result<Outcome, ExecuteError>) -> Self {
        Running(State::Ended(Some(result)))
    }

    /// Takes the outcome of an attempt that ended when it started.
    pub(crate) fn take_ended(&mut self) -> Option<Result<Outcome, ExecuteError>> {
        match &mut self.0 {
            State::Ended(result) => result.take(),
            State::Future(_) => None,
        }
    }
}

impl Future for Running {
    type Output = Result<Outcome, ExecuteError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        match &mut self.0 {
            State::Ended(result) => Poll::Ready(result.take().unwrap_or_else(|| {
                Err(ExecuteError::Executor {
                    task: String::new(),
                    message: "the outcome of an attempt was taken twice".into(),
                })
            })),
            State::Future(future) => future.as_mut().poll(context),
        }
    }
}

/// Runs tasks. The engine calls [`Executor::run`] on the thread of the driver, and drives the
/// future it returns on Tokio, so that many attempts run at the same time.
pub trait Executor {
    fn run(&mut self, attempt: Attempt) -> Running;
}

/// The Tokio runtime that drives the attempts of all runs in this process.
pub(crate) fn runtime() -> Result<&'static tokio::runtime::Runtime, ExecuteError> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    let runtime = RUNTIME.get_or_init(|| {
        let workers = std::thread::available_parallelism().map_or(2, |cpus| cpus.get().min(4));
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers)
            .thread_name("iunctus-attempts")
            .enable_all()
            .build()
            .map_err(|error| error.to_string())
    });
    runtime.as_ref().map_err(|message| ExecuteError::Executor {
        task: String::new(),
        message: format!("cannot start the Tokio runtime of the engine: {message}"),
    })
}
