//! Defines executor admission, running attempts and task completion outcomes.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};

pub use tokio_util::sync::CancellationToken;

use crate::{ExecuteError, Files, Outcome, Task, TaskId};

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
    /// The attempt ended when it started; `None` once the outcome was taken. Boxed, as an
    /// outcome is much larger than a future.
    Ended(Option<Box<Result<Outcome, ExecuteError>>>),
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
        Running(State::Ended(Some(Box::new(result))))
    }

    /// Takes the outcome of an attempt that ended when it started.
    pub(crate) fn take_ended(&mut self) -> Option<Result<Outcome, ExecuteError>> {
        match &mut self.0 {
            State::Ended(result) => result.take().map(|result| *result),
            State::Future(_) => None,
        }
    }
}

impl Future for Running {
    type Output = Result<Outcome, ExecuteError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        match &mut self.0 {
            State::Ended(result) => Poll::Ready(result.take().map_or_else(
                || {
                    Err(ExecuteError::Executor {
                        task: String::new(),
                        message: "the outcome of an attempt was taken twice".into(),
                    })
                },
                |result| *result,
            )),
            State::Future(future) => future.as_mut().poll(context),
        }
    }
}

/// Runs tasks. The engine calls [`Executor::run`] on the thread of the driver, and drives the
/// future it returns on Tokio, so that many attempts run at the same time.
pub trait Executor {
    fn run(&mut self, attempt: Attempt) -> Running;

    /// Settles only this task and attempt after native validation, with build leases released
    /// beforehand.
    fn settle(
        &mut self,
        _task: TaskId,
        _attempt: u32,
        _accepted: bool,
    ) -> Pin<Box<dyn Future<Output = Result<(), ExecuteError>> + Send + 'static>> {
        Box::pin(async { Ok(()) })
    }

    /// The files that the tasks made, to deliver them. `None` for an executor whose files are
    /// made up, like the simulator: a simulated run delivers nothing.
    fn files(&self) -> Option<Arc<dyn Files>> {
        None
    }

    /// Starts no attempt that has not begun to run: attempts that still wait for their turn,
    /// like for their share of the machine, end at once as cancelled. The engine holds the
    /// executor when its driver drains. The default holds nothing.
    fn hold(&mut self) {}

    /// Waits until the daemon tasks that the run stopped ended, before the run ends.
    fn release(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async {})
    }
}

/// The Tokio runtime that drives the attempts of all runs in this process. Executors make
/// clients that attempts use, like one of the Kubernetes API, on it.
pub fn runtime() -> Result<&'static tokio::runtime::Runtime, ExecuteError> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    let runtime = RUNTIME.get_or_init(|| {
        let workers = std::thread::available_parallelism().map_or(2, |cpus| cpus.get().min(4));
        // Attempts run deep driver code; debug builds overflow the default stack of 2 MiB.
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers)
            .thread_name("uwe-attempts")
            .thread_stack_size(8 << 20)
            .enable_all()
            .build()
            .map_err(|error| error.to_string())
    });
    runtime.as_ref().map_err(|message| ExecuteError::Executor {
        task: String::new(),
        message: format!("cannot start the Tokio runtime of the engine: {message}"),
    })
}
