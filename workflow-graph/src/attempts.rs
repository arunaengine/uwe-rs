//! The attempts in flight: each runs on Tokio and reports its end over a channel to the thread
//! of the driver.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

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
            cancel,
        })
    }

    /// Lets an attempt run. One that ended when it started is its end at once; the others
    /// report their end to [`Attempts::next`].
    pub(crate) fn start(&self, id: TaskId, attempt: u32, running: Running) -> Option<Ended> {
        let running = match running.into_ended() {
            Ok(result) => {
                return Some(Ended {
                    id,
                    attempt,
                    result,
                });
            }
            Err(running) => running,
        };
        let sender = self.sender.clone();
        self.runtime.spawn(async move {
            let result = running.await;
            // The receiver is gone only when the run has ended, and then nobody waits.
            let _ = sender.send(Ended {
                id,
                attempt,
                result,
            });
        });
        None
    }

    /// Waits for the next attempt to end, unless the run is cancelled first.
    pub(crate) fn next(&mut self) -> Result<Ended, ExecuteError> {
        let cancel = self.cancel.clone();
        let receiver = &mut self.receiver;
        self.runtime.block_on(async {
            tokio::select! {
                ended = receiver.recv() => ended.ok_or(ExecuteError::Cancelled),
                () = cancel.cancelled() => Err(ExecuteError::Cancelled),
            }
        })
    }

    /// Waits for the next attempt to end, at most until `deadline`, after the run stopped.
    pub(crate) fn next_until(&mut self, deadline: Instant) -> Option<Ended> {
        let wait = deadline.saturating_duration_since(Instant::now());
        let receiver = &mut self.receiver;
        // The timer must be made inside the runtime.
        let ended = self
            .runtime
            .block_on(async { tokio::time::timeout(wait, receiver.recv()).await });
        ended.ok().flatten()
    }
}
