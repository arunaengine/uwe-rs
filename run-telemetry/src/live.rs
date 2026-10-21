//! Exports committed task spans in bounded batches and drains the final run span.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use run_database::{Database, DatabaseError, Recorder};

use crate::spans::{finished_run, finished_spans, request};
use crate::{SPANS_PER_REQUEST, Span, send};

const INTERVAL: Duration = Duration::from_secs(2);
const DRAIN_LIMIT: Duration = Duration::from_secs(5);

/// Owns the export worker. Close the recorder before draining this handle.
pub struct Live {
    done: Receiver<()>,
    thread: Option<JoinHandle<()>>,
}

impl Live {
    pub fn start(
        path: &Path,
        recorder: &mut Recorder,
        client: reqwest::Client,
        endpoint: String,
        headers: Vec<(String, String)>,
    ) -> Result<Self, std::io::Error> {
        let committed = recorder.commits();
        let run = recorder.run().to_owned();
        let path = path.to_owned();
        let (done, completed) = mpsc::channel();
        let dispatch = tracing::dispatcher::get_default(Clone::clone);
        let thread = std::thread::Builder::new()
            .name("iunctus-telemetry".into())
            .spawn(move || {
                let result = tracing::dispatcher::with_default(&dispatch, || {
                    export(path, &run, committed, client, endpoint, headers)
                });
                if let Err(error) = result {
                    tracing::warn!("live telemetry stopped: {error}");
                }
                let _ = done.send(());
            })?;
        Ok(Self {
            done: completed,
            thread: Some(thread),
        })
    }

    /// Waits at most five seconds for pending spans; export never changes the run result.
    pub fn finish(mut self) {
        self.drain();
    }

    fn drain(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        if self.done.recv_timeout(DRAIN_LIMIT).is_ok() {
            let _ = thread.join();
        } else {
            tracing::warn!("live telemetry drain deadline elapsed; pending spans may be lost");
        }
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.drain();
    }
}

struct Exporter {
    runtime: tokio::runtime::Runtime,
    client: reqwest::Client,
    endpoint: String,
    headers: Vec<(String, String)>,
    cursor: i64,
    tasks: HashSet<u64>,
    pending: Vec<Span>,
}

fn export(
    path: PathBuf,
    run: &str,
    committed: Receiver<()>,
    client: reqwest::Client,
    endpoint: String,
    headers: Vec<(String, String)>,
) -> Result<(), DatabaseError> {
    let database = Database::open(&path)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut exporter = Exporter {
        runtime,
        client,
        endpoint,
        headers,
        cursor: 0,
        tasks: HashSet::new(),
        pending: Vec::new(),
    };
    loop {
        let deadline = Instant::now() + INTERVAL;
        let finalizing = loop {
            match committed.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(()) => continue,
                Err(RecvTimeoutError::Timeout) => break false,
                Err(RecvTimeoutError::Disconnected) => break true,
            }
        };
        exporter.collect(&database, run)?;
        if finalizing {
            // The recorder commits before closing its notification sender.
            for task in database.tasks(run)? {
                exporter.task(&database, run, task.id, true)?;
            }
            exporter.flush();
            if let Some(span) = database.snapshot(|database| finished_run(database, run))? {
                exporter.push(span);
            }
            exporter.flush();
            return Ok(());
        }
        exporter.flush();
    }
}

impl Exporter {
    fn collect(&mut self, database: &Database, run: &str) -> Result<(), DatabaseError> {
        loop {
            let events = database.events(run, self.cursor, SPANS_PER_REQUEST as u32)?;
            for event in &events {
                if matches!(event.kind.as_str(), "finished" | "validated")
                    && let Some(task) = event.task
                {
                    self.task(database, run, task, false)?;
                }
                self.cursor = event.sequence;
            }
            if events.len() < SPANS_PER_REQUEST {
                return Ok(());
            }
        }
    }

    fn task(
        &mut self,
        database: &Database,
        run: &str,
        task: u64,
        finalizing: bool,
    ) -> Result<(), DatabaseError> {
        if self.tasks.contains(&task) {
            return Ok(());
        }
        let spans =
            database.snapshot(|database| finished_spans(database, run, task, finalizing))?;
        if !spans.is_empty() {
            // Never retry an ambiguous HTTP result, which could duplicate accepted spans.
            self.tasks.insert(task);
            for span in spans {
                self.push(span);
            }
        }
        Ok(())
    }

    fn push(&mut self, span: Span) {
        self.pending.push(span);
        if self.pending.len() == SPANS_PER_REQUEST {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let requests = [request(std::mem::take(&mut self.pending))];
        let result = self.runtime.block_on(async {
            tokio::time::timeout(
                INTERVAL,
                send(&self.client, &self.endpoint, &self.headers, &requests),
            )
            .await
        });
        match result {
            Ok(Ok(sent)) if sent.rejected > 0 => tracing::warn!(
                rejected = sent.rejected,
                "collector rejected telemetry spans"
            ),
            Ok(Ok(_)) => {}
            Ok(Err(_)) => {
                tracing::warn!("live telemetry export failed; batch dropped (details redacted)")
            }
            Err(_) => tracing::warn!("live telemetry export deadline elapsed; batch dropped"),
        }
    }
}
