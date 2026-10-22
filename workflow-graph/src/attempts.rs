//! The attempts in flight: each runs on Tokio and reports its end over a channel to the thread
//! of the driver.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::time::Instant;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::executor::{CancellationToken, Running, runtime};
use crate::{ExecuteError, Outcome, TaskId};

/// How an attempt ended.
pub(crate) struct Ended {
    pub(crate) id: TaskId,
    pub(crate) attempt: u32,
    pub(crate) result: Result<Outcome, ExecuteError>,
}

pub(crate) struct Attempts {
    runtime: &'static tokio::runtime::Runtime,
    sender: UnboundedSender<Ended>,
    receiver: UnboundedReceiver<Ended>,
    handles: HashMap<(TaskId, u32), tokio::task::JoinHandle<()>>,
    /// Fires when the run stops; every attempt gets it.
    pub(crate) cancel: CancellationToken,
}

impl Attempts {
    pub(crate) fn new(cancel: CancellationToken) -> Result<Self, ExecuteError> {
        let (sender, receiver) = unbounded_channel();
        Ok(Attempts {
            runtime: runtime()?,
            sender,
            receiver,
            handles: HashMap::new(),
            cancel,
        })
    }

    /// Lets an attempt run. One that ended when it started is its end at once; the others
    /// report their end to [`Attempts::next`].
    pub(crate) fn start(
        &mut self,
        id: TaskId,
        attempt: u32,
        mut running: Running,
    ) -> Option<Ended> {
        if let Some(result) = running.take_ended() {
            return Some(Ended {
                id,
                attempt,
                result,
            });
        }
        let sender = self.sender.clone();
        let handle = self.runtime.spawn(async move {
            let result = tokio::spawn(running).await.unwrap_or_else(|error| {
                Err(ExecuteError::Executor {
                    task: id.to_string(),
                    message: format!("attempt {attempt} failed to join: {error}"),
                })
            });
            // The receiver is gone only when the run has ended, and then nobody waits.
            let _ = sender.send(Ended {
                id,
                attempt,
                result,
            });
        });
        self.handles.insert((id, attempt), handle);
        None
    }

    /// Waits for the next attempt to end, unless the run is cancelled first.
    pub(crate) fn next(&mut self) -> Result<Ended, ExecuteError> {
        let cancel = self.cancel.clone();
        let receiver = &mut self.receiver;
        let ended = self.runtime.block_on(async {
            tokio::select! {
                ended = receiver.recv() => ended.ok_or(ExecuteError::Cancelled),
                () = cancel.cancelled() => Err(ExecuteError::Cancelled),
            }
        })?;
        self.join(&ended);
        Ok(ended)
    }

    /// Waits for an outcome or a driver timer while retaining run cancellation.
    pub(crate) fn next_wake(&mut self, deadline: Instant) -> Result<Option<Ended>, ExecuteError> {
        let cancel = self.cancel.clone();
        let receiver = &mut self.receiver;
        let wait = deadline.saturating_duration_since(Instant::now());
        let ended = self.runtime.block_on(async {
            tokio::select! {
                () = cancel.cancelled() => Err(ExecuteError::Cancelled),
                ended = receiver.recv() => ended.map(Some).ok_or(ExecuteError::Cancelled),
                () = tokio::time::sleep(wait) => Ok(None),
            }
        })?;
        if let Some(ended) = &ended {
            self.join(ended);
        }
        Ok(ended)
    }

    /// Cancellation requests cleanup; only the attempt's completion releases its ownership.
    pub(crate) fn next_stopped(&mut self) -> Option<Ended> {
        if self.handles.is_empty() {
            return None;
        }
        let ended = self.runtime.block_on(self.receiver.recv())?;
        self.join(&ended);
        Some(ended)
    }

    fn join(&mut self, ended: &Ended) {
        if let Some(handle) = self.handles.remove(&(ended.id, ended.attempt)) {
            let _ = self.runtime.block_on(handle);
        }
    }
}

impl Drop for Attempts {
    fn drop(&mut self) {
        if !self.handles.is_empty() {
            self.cancel.cancel();
            self.runtime.block_on(async {
                for (_, handle) in self.handles.drain() {
                    let _ = handle.await;
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn drop_joins_cleanup() {
        let cancel = CancellationToken::new();
        let stopped = cancel.clone();
        let (cleaning, acknowledged) = mpsc::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let (done, completed) = mpsc::channel();
        let owner = std::thread::spawn(move || {
            let mut attempts = Attempts::new(cancel).unwrap();
            attempts.start(
                TaskId(1),
                1,
                Running::new(async move {
                    stopped.cancelled().await;
                    cleaning.send(()).unwrap();
                    released.await.unwrap();
                    Ok(Outcome::default())
                }),
            );
            drop(attempts);
            done.send(()).unwrap();
        });
        acknowledged.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(
            completed.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        release.send(()).unwrap();
        completed.recv_timeout(Duration::from_secs(5)).unwrap();
        owner.join().unwrap();
    }
}
