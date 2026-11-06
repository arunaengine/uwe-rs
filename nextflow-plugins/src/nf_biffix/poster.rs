//! Posts nf-biffix events in batches to the run's ingest route, and accounts for the events it
//! could not deliver.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::service::{self, Sender};

const FLUSH: Duration = Duration::from_millis(1000);
const BATCH_EVENTS: usize = 500;
const BATCH_BYTES: usize = 4 * 1024 * 1024;
const LIMIT: Duration = Duration::from_millis(5000);

pub(super) struct Poster {
    url: String,
    token: String,
    sender: Option<Sender>,
    queue: VecDeque<Value>,
    abandoned: u64,
    flushed: Instant,
}

impl Poster {
    pub(super) fn new(url: String, token: String) -> Poster {
        Poster {
            url,
            token,
            sender: None,
            queue: VecDeque::new(),
            abandoned: 0,
            flushed: Instant::now(),
        }
    }

    pub(super) fn push(&mut self, event: Value) {
        self.queue.push_back(event);
    }

    /// Posts what is queued once the flush interval passed.
    pub(super) fn tick(&mut self) {
        if self.flushed.elapsed() >= FLUSH {
            self.flush();
        }
    }

    /// Sends what is left, and reports the events that were lost.
    pub(super) fn close(&mut self) {
        self.flush();
        if self.abandoned > 0 {
            tracing::warn!(
                "biffix ingest: {} event(s) not delivered ({} abandoned at close or while ingest \
                 was unreachable, 0 dropped from a full buffer); the coordinator's events file \
                 replay re-sends them",
                self.abandoned,
                self.abandoned
            );
        }
    }

    fn flush(&mut self) {
        self.flushed = Instant::now();
        while !self.queue.is_empty() {
            let mut batch = Vec::new();
            let mut bytes = 0;
            while let Some(event) = self.queue.front() {
                let size = event.to_string().len() + 1;
                if !batch.is_empty() && (batch.len() >= BATCH_EVENTS || bytes + size > BATCH_BYTES)
                {
                    break;
                }
                bytes += size;
                batch.extend(self.queue.pop_front());
            }
            if let Err(detail) = self.post(&batch) {
                tracing::warn!(
                    "biffix ingest POST of {} event(s) to {} failed; dropped them (the events \
                     file replay re-sends them): {detail}",
                    batch.len(),
                    service::safe_url(&self.url)
                );
                self.abandoned += batch.len() as u64;
            }
        }
    }

    fn post(&mut self, batch: &[Value]) -> Result<(), String> {
        let sender = service::sender(&mut self.sender)?;
        let body = serde_json::json!({"events": batch}).to_string();
        let request = (sender.client().post(&self.url))
            .timeout(LIMIT)
            .bearer_auth(&self.token)
            .header("Content-Type", "application/json")
            .body(body);
        let answer = sender.send(request).map_err(|error| error.to_string())?;
        if (200..300).contains(&answer.status) {
            return Ok(());
        }
        let body: String = answer.body.split_whitespace().collect::<Vec<_>>().join(" ");
        Err(format!(
            "HTTP {}: {}",
            answer.status,
            body.chars().take(200).collect::<String>()
        ))
    }
}
