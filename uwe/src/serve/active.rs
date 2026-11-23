//! The runs that the studio started: their phase, cancellation, live events and threads.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use run_page::{LaunchRequest, Launched, Phase, Refusal};
use serde_json::{Value, json};
use tokio::sync::{broadcast, oneshot};
use workflow_graph::CancellationToken;

use crate::run::{Event, Listener, PrintSink};

/// How many live events wait for a slow page before it misses some, which it is told.
const LIVE: usize = 1024;

/// How many runs that ended the registry keeps for the page and WES, so that a server that
/// runs for long does not grow; the run store keeps every run.
const ENDED: usize = 256;

/// The runs of this process that the studio started, the latest last.
#[derive(Default)]
pub(super) struct Registry {
    runs: Mutex<Vec<Entry>>,
}

struct Entry {
    launched: Launched,
    cancel: CancellationToken,
    /// The live events while the run goes on; dropped at its end, which ends the streams.
    live: Option<broadcast::Sender<Value>>,
    /// Tells the launch whether the run reached the run store or failed before.
    started: Option<oneshot::Sender<Result<(), String>>>,
    thread: Option<JoinHandle<()>>,
}

/// What a run needs to report to the registry while it goes on.
pub(super) struct Admitted {
    pub cancel: CancellationToken,
    pub listener: Listener,
    pub sink: PrintSink,
    /// Answers once the run is in the run store, or with why it failed before.
    pub started: oneshot::Receiver<Result<(), String>>,
}

/// Records the end of a run when its thread ends, also when the run panics, so that no request
/// waits for a run that never starts and no live stream stays open.
pub(super) struct Ending {
    registry: Arc<Registry>,
    run: String,
    /// The error that the run ended with, once it ended without a panic.
    error: Option<Option<String>>,
}

impl Ending {
    pub(super) fn finish(mut self, error: Option<String>) {
        self.error = Some(error);
    }
}

impl Drop for Ending {
    fn drop(&mut self) {
        let error =
            (self.error.take()).unwrap_or_else(|| Some("the run ended unexpectedly".into()));
        self.registry.ended(&self.run, error);
    }
}

impl Registry {
    /// Adds a run that starts now with `request`, unless `limit` runs go on already.
    pub(super) fn admit(
        self: &Arc<Self>,
        run: &str,
        request: LaunchRequest,
        limit: usize,
    ) -> Result<Admitted, Refusal> {
        let (live, _) = broadcast::channel(LIVE);
        let (told, started) = oneshot::channel();
        let cancel = CancellationToken::new();
        let mut entries = self.entries();
        let active = entries.iter().filter(|entry| !entry.launched.phase.ended());
        if active.count() >= limit {
            return Err(Refusal::busy(format!(
                "this server runs {limit} runs already; start this one when one has ended"
            )));
        }
        entries.push(Entry {
            launched: Launched {
                run: run.to_owned(),
                phase: Phase::Queued,
                error: None,
                time: now(),
                request,
            },
            cancel: cancel.clone(),
            live: Some(live.clone()),
            started: Some(told),
            thread: None,
        });
        drop(entries);
        let registry = self.clone();
        let listener: Listener = Arc::new(move |event: Event| registry.heard(event));
        let (registry, id) = (self.clone(), run.to_owned());
        let sink = PrintSink::new(move |text: &str| {
            let event = json!({"kind": "print", "run": id, "text": text, "time": now()});
            registry.send(&id, event);
        });
        Ok(Admitted {
            cancel,
            listener,
            sink,
            started,
        })
    }

    /// The guard that the thread of a run holds until the run ends.
    pub(super) fn ending(self: &Arc<Self>, run: &str) -> Ending {
        Ending {
            registry: self.clone(),
            run: run.to_owned(),
            error: None,
        }
    }

    /// Keeps the thread of a run, so that `stop` can wait for it.
    pub(super) fn keep(&self, run: &str, thread: JoinHandle<()>) {
        if let Some(entry) = self
            .entries()
            .iter_mut()
            .find(|entry| entry.launched.run == run)
        {
            entry.thread = Some(thread);
        }
    }

    /// Records how a run ended, and ends its live streams. Its thread has recorded it, so the
    /// oldest runs that ended beyond [`ENDED`] leave the registry.
    pub(super) fn ended(&self, run: &str, error: Option<String>) {
        let mut entries = self.entries();
        let Some(entry) = entries.iter_mut().find(|entry| entry.launched.run == run) else {
            return;
        };
        let canceled = entry.cancel.is_cancelled();
        entry.launched.phase = match (&error, canceled) {
            (_, true) => Phase::Canceled,
            (None, false) => Phase::Complete,
            (Some(_), false) => Phase::Failed,
        };
        if let Some(started) = entry.started.take() {
            let _ = started.send(Err(error.clone().unwrap_or_else(|| "the run ended".into())));
        }
        entry.launched.error = error;
        entry.live = None;
        evict(&mut entries);
    }

    /// Asks a run to stop; false when it is not one of these runs or has ended.
    pub(super) fn cancel(&self, run: &str) -> bool {
        let mut entries = self.entries();
        let Some(entry) = (entries.iter_mut()).find(|entry| entry.launched.run == run) else {
            return false;
        };
        if !matches!(entry.launched.phase, Phase::Queued | Phase::Running) {
            return false;
        }
        entry.launched.phase = Phase::Canceling;
        entry.cancel.cancel();
        true
    }

    pub(super) fn launched(&self, run: &str) -> Option<Launched> {
        let entries = self.entries();
        let entry = entries.iter().find(|entry| entry.launched.run == run)?;
        Some(entry.launched.clone())
    }

    /// Every run, the latest first.
    pub(super) fn launches(&self) -> Vec<Launched> {
        let entries = self.entries();
        entries
            .iter()
            .rev()
            .map(|entry| entry.launched.clone())
            .collect()
    }

    pub(super) fn follow(&self, run: &str) -> Option<broadcast::Receiver<Value>> {
        let entries = self.entries();
        let entry = entries.iter().find(|entry| entry.launched.run == run)?;
        entry.live.as_ref().map(broadcast::Sender::subscribe)
    }

    /// Asks every run that goes on to stop and waits until their threads have recorded them.
    /// The number of runs it waited for.
    pub(super) fn stop(&self) -> usize {
        let threads: Vec<JoinHandle<()>> = {
            let mut entries = self.entries();
            for entry in entries.iter() {
                entry.cancel.cancel();
            }
            entries
                .iter_mut()
                .filter_map(|entry| entry.thread.take())
                .collect()
        };
        let count = threads.len();
        for thread in threads {
            let _ = thread.join();
        }
        count
    }

    fn heard(&self, event: Event) {
        match &event {
            Event::Started { run, .. } => {
                let mut entries = self.entries();
                if let Some(entry) = entries.iter_mut().find(|entry| entry.launched.run == *run) {
                    if entry.launched.phase == Phase::Queued {
                        entry.launched.phase = Phase::Running;
                    }
                    if let Some(started) = entry.started.take() {
                        let _ = started.send(Ok(()));
                    }
                }
            }
            Event::Log { run, .. } | Event::Usage { run, .. } => {
                let run = run.clone();
                if let Ok(value) = serde_json::to_value(&event) {
                    self.send(&run, value);
                }
            }
            _ => {}
        }
    }

    /// Sends a live event to the pages that follow the run; nothing when none does.
    fn send(&self, run: &str, event: Value) {
        let entries = self.entries();
        let entry = entries.iter().find(|entry| entry.launched.run == run);
        if let Some(live) = entry.and_then(|entry| entry.live.as_ref()) {
            let _ = live.send(event);
        }
    }

    /// The entries, also after a thread panicked while it held them.
    fn entries(&self) -> MutexGuard<'_, Vec<Entry>> {
        self.runs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Drops the oldest runs that ended, beyond the [`ENDED`] latest ones.
fn evict(entries: &mut Vec<Entry>) {
    let ended = |entry: &Entry| entry.launched.phase.ended() && entry.live.is_none();
    let mut surplus = entries.iter().filter(|entry| ended(entry)).count();
    surplus = surplus.saturating_sub(ENDED);
    entries.retain(|entry| {
        let dropped = surplus > 0 && ended(entry);
        surplus -= usize::from(dropped);
        !dropped
    });
}

fn now() -> i64 {
    let elapsed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    elapsed.map_or(0, |elapsed| elapsed.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_ended_runs() {
        let registry = Arc::new(Registry::default());
        let request = LaunchRequest::default();
        let _running = registry.admit("running", request.clone(), 2);
        for number in 0..ENDED + 2 {
            let run = format!("run-{number}");
            let _admitted = registry.admit(&run, request.clone(), 2);
            registry.ended(&run, None);
        }
        let kept = registry.launches();
        assert_eq!(
            kept.len(),
            ENDED + 1,
            "the runs that ended beyond the limit leave"
        );
        assert!(
            registry.launched("running").is_some(),
            "a running run stays"
        );
        assert!(registry.launched("run-0").is_none() && registry.launched("run-1").is_none());
        let latest = format!("run-{}", ENDED + 1);
        assert_eq!(
            registry.launched(&latest).map(|run| run.phase),
            Some(Phase::Complete)
        );
    }

    #[test]
    fn limits_active_runs() {
        let registry = Arc::new(Registry::default());
        let request = LaunchRequest::default();
        assert!(registry.admit("first", request.clone(), 2).is_ok());
        assert!(registry.admit("second", request.clone(), 2).is_ok());
        let refused = registry.admit("third", request.clone(), 2).err();
        assert_eq!(refused.map(|refused| refused.status.as_u16()), Some(429));
        registry.ended("first", None);
        assert!(registry.admit("third", request, 2).is_ok());
    }

    #[test]
    fn panicked_runs_end() {
        let registry = Arc::new(Registry::default());
        let admitted = registry.admit("lost", LaunchRequest::default(), 1);
        let admitted = admitted.expect("admitted");
        let ending = registry.ending("lost");
        let thread = std::thread::spawn(move || {
            let _ending = ending;
            panic!("the run breaks");
        });
        assert!(thread.join().is_err());
        let told = admitted.started.blocking_recv().expect("an answer");
        assert_eq!(told, Err("the run ended unexpectedly".to_owned()));
        let launched = registry.launched("lost").expect("the run");
        assert_eq!(launched.phase, Phase::Failed);
        assert!(registry.follow("lost").is_none(), "its live stream ended");
    }
}
