//! Retained cache evidence awaiting a native driver's decision for one attempt.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use content_store::{EntrySnapshot, Holder, layout};
use executor::{Backend, ExecutorError};
use task_model::Digest;
use workflow_graph::{CancellationToken, Task};

use crate::{CacheEntry, Runner, RunnerError};

pub(crate) struct Candidate {
    pub namespace: String,
    pub key: Digest,
    pub source: Source,
}

pub(crate) enum Source {
    Publish(CacheEntry),
    Hit(EntrySnapshot),
}

pub(crate) struct Pending {
    pub task: Arc<Task>,
    pub cancel: CancellationToken,
    pub candidate: Mutex<Option<Candidate>>,
    pub terminal: AtomicBool,
}

impl Pending {
    pub fn finish(&self) {
        self.terminal.store(true, Ordering::Release);
    }

    pub fn terminal(&self) -> bool {
        self.terminal.load(Ordering::Acquire)
    }

    pub fn take(self: &Arc<Self>) -> Option<Settlement> {
        let candidate = self
            .candidate
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()?;
        Some(Settlement {
            pending: self.clone(),
            candidate: Some(candidate),
        })
    }
}

pub(crate) struct Completing {
    pub pending: Arc<Pending>,
    pub retained: bool,
}

impl Drop for Completing {
    fn drop(&mut self) {
        if !self.retained || self.pending.cancel.is_cancelled() {
            self.pending.cancel.cancel();
            self.pending.finish();
        }
    }
}

pub(crate) struct Settlement {
    pub pending: Arc<Pending>,
    pub candidate: Option<Candidate>,
}

impl Drop for Settlement {
    fn drop(&mut self) {
        // Failed or interrupted I/O may already have mutated a remote store. Keep its exact
        // evidence for another settlement, but never revive a cancelled or superseded attempt.
        if self.pending.cancel.is_cancelled() {
            self.pending.finish();
        }
        if !self.pending.terminal() && self.candidate.is_some() {
            *self
                .pending
                .candidate
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = self.candidate.take();
        }
    }
}

impl<B: Backend> Runner<B> {
    pub(crate) async fn settle(
        &self,
        candidate: &Candidate,
        accepted: bool,
        cancel: &CancellationToken,
    ) -> Result<(), RunnerError> {
        let entry = match (&candidate.source, accepted) {
            (Source::Publish(entry), true) => Some(entry),
            (Source::Hit(_), false) => None,
            _ => return Ok(()),
        };
        let settled = self.settle_guarded(candidate, accepted, cancel).await;
        match (settled, entry) {
            // Missing a cache write is harmless: a later run only runs the task again.
            (Err(error), Some(entry)) if !cancel.is_cancelled() => {
                eprintln!(
                    "iunctus: warning: the result of task {} is not cached: {error}",
                    entry.task
                );
                Ok(())
            }
            (settled, _) => settled,
        }
    }

    /// Settles under the gate; the time limit starts once the gate is held, as clean-up may
    /// hold it for long.
    async fn settle_guarded(
        &self,
        candidate: &Candidate,
        accepted: bool,
        cancel: &CancellationToken,
    ) -> Result<(), RunnerError> {
        let guard = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ExecutorError::Cancelled.into()),
            guard = self.admit() => guard?,
        };
        let work = async {
            match (&candidate.source, accepted) {
                (Source::Publish(entry), true) => {
                    let name = format!(
                        "tasks/{}/{}",
                        layout::namespace_name(&candidate.namespace),
                        candidate.key.hex()
                    );
                    let holder = Holder::new(&entry.run, &entry.task);
                    // Do not wait for another build: ordered outcome handling may be what
                    // allows that build to finish. Missing this cache write is harmless.
                    let lease = tokio::time::timeout(
                        Duration::from_secs(5),
                        self.phase(
                            None,
                            Some(&guard),
                            Some(cancel),
                            self.store.try_lease(&name, &holder),
                        ),
                    )
                    .await
                    .map_err(|_| timeout())??;
                    let Some(lease) = lease else {
                        return Ok(());
                    };
                    let bytes = entry.to_bytes(&candidate.key)?;
                    self.phase(
                        Some(&lease),
                        Some(&guard),
                        Some(cancel),
                        self.store
                            .put_entry(&candidate.namespace, &candidate.key, bytes.into()),
                    )
                    .await?;
                }
                (Source::Hit(observed), false) => {
                    self.phase(
                        None,
                        Some(&guard),
                        Some(cancel),
                        self.store
                            .invalidate_entry(&candidate.namespace, &candidate.key, observed),
                    )
                    .await?;
                }
                _ => {}
            }
            Ok(())
        };
        // Lease checks diagnose loss; they are not an atomic distributed publication fence.
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(ExecutorError::Cancelled.into()),
            result = tokio::time::timeout(Duration::from_secs(10), work) => result.map_err(|_| timeout())?,
        }
    }
}

pub(crate) fn timeout() -> RunnerError {
    std::io::Error::new(std::io::ErrorKind::TimedOut, "cache settlement timed out").into()
}
