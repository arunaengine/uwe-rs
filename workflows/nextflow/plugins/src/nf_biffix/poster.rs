//! Posts nf-biffix events in batches to the run's ingest route, within the limits of
//! `ingest-limits.json`, and accounts for the events it could not deliver.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::service::{self, Sender};

const FLUSH: Duration = Duration::from_millis(1000);
const BATCH_EVENTS: usize = 500;
const BATCH_BYTES: usize = 4 * 1024 * 1024;
const BUFFER: usize = 20_000;
const LIMIT: Duration = Duration::from_millis(5000);
const ATTEMPTS: u32 = 3;
const BACKOFF: Duration = Duration::from_millis(500);
const RETRY_MOST: Duration = Duration::from_millis(30_000);
const SPLIT_BUDGET: u32 = 64;
const DROP_WARNING: Duration = Duration::from_millis(30_000);
/// The kinds that a full buffer keeps longest.
const KEEP: [&str; 9] = [
    "task_submitted",
    "task_succeeded",
    "task_failed",
    "task_retrying",
    "task_cached",
    "run_started",
    "run_succeeded",
    "run_failed",
    "engine_task_verdict",
];

pub(super) struct Poster {
    url: String,
    token: String,
    sender: Option<Sender>,
    queue: VecDeque<Value>,
    /// Posting stopped after the ingest route refused the token.
    disabled: bool,
    abandoned: u64,
    dropped: u64,
    flushed: Instant,
    warned: Option<Instant>,
}

/// How one POST ended.
enum Sent {
    Done,
    /// Retry the same batch.
    Again(Option<Duration>),
    /// Split the batch, or drop its single event.
    Rejected(String),
    Denied(String),
    Failed(String),
}

impl Poster {
    pub(super) fn new(url: String, token: String) -> Poster {
        Poster {
            url,
            token,
            sender: None,
            queue: VecDeque::new(),
            disabled: false,
            abandoned: 0,
            dropped: 0,
            flushed: Instant::now(),
            warned: None,
        }
    }

    /// Queues an event; a full buffer drops the oldest descriptive events first.
    pub(super) fn push(&mut self, event: Value) {
        if self.disabled {
            return;
        }
        self.queue.push_back(event);
        if self.queue.len() <= BUFFER {
            return;
        }
        let descriptive = (self.queue.iter())
            .position(|event| !KEEP.contains(&event["kind"].as_str().unwrap_or_default()));
        self.queue.remove(descriptive.unwrap_or(0));
        self.dropped += 1;
        if self
            .warned
            .is_none_or(|warned| warned.elapsed() >= DROP_WARNING)
        {
            self.warned = Some(Instant::now());
            tracing::warn!(
                "biffix ingest buffer full (ingest unreachable?); {} event(s) dropped so far, \
                 oldest descriptive events first (the events file replay re-sends them)",
                self.dropped
            );
        }
    }

    /// Posts what is queued once the flush interval passed.
    pub(super) fn tick(&mut self) {
        if self.flushed.elapsed() >= FLUSH {
            self.flush(ATTEMPTS);
        }
    }

    /// Sends what is left once, without retries, and reports the events that were lost.
    pub(super) fn close(&mut self) {
        self.flush(1);
        let lost = self.abandoned + self.dropped;
        if lost > 0 {
            tracing::warn!(
                "biffix ingest: {lost} event(s) not delivered ({} abandoned at close or while \
                 ingest was unreachable, {} dropped from a full buffer); the coordinator's \
                 events file replay re-sends them",
                self.abandoned,
                self.dropped
            );
        }
    }

    fn flush(&mut self, attempts: u32) {
        self.flushed = Instant::now();
        while !self.queue.is_empty() && !self.disabled {
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
            let mut budget = SPLIT_BUDGET;
            self.deliver(batch, attempts, &mut budget);
        }
        if self.disabled {
            self.abandoned += self.queue.len() as u64;
            self.queue.clear();
        }
    }

    /// Posts a batch with retries; a batch that the route rejects is split in halves.
    fn deliver(&mut self, batch: Vec<Value>, attempts: u32, budget: &mut u32) {
        let shown = service::safe_url(&self.url);
        let mut attempt = 0;
        loop {
            if self.disabled {
                self.abandoned += batch.len() as u64;
                return;
            }
            attempt += 1;
            *budget = budget.saturating_sub(1);
            let failure = match self.post(&batch) {
                Sent::Done => return,
                Sent::Again(wait) if attempt < attempts => {
                    std::thread::sleep(wait.unwrap_or(BACKOFF * 2u32.pow(attempt - 1)));
                    continue;
                }
                Sent::Again(_) => "the ingest route did not answer in time".to_owned(),
                Sent::Rejected(_) if batch.len() > 1 && *budget > 0 => {
                    let mut first = batch;
                    let second = first.split_off(first.len() / 2);
                    self.deliver(first, attempts, budget);
                    self.deliver(second, attempts, budget);
                    return;
                }
                Sent::Rejected(detail) => {
                    for event in &batch {
                        tracing::warn!(
                            "biffix ingest POST to {shown} rejected one {} event ({}), \
                             dropped: {detail}",
                            text(&event["kind"]),
                            text(&event["task_key"])
                        );
                    }
                    self.abandoned += batch.len() as u64;
                    return;
                }
                Sent::Denied(detail) => {
                    self.disabled = true;
                    tracing::warn!(
                        "biffix ingest disabled: ingest refused the capability token ({detail}); \
                         later events go only to the events file"
                    );
                    self.abandoned += batch.len() as u64;
                    return;
                }
                Sent::Failed(detail) => detail,
            };
            tracing::warn!(
                "biffix ingest POST of {} event(s) to {shown} failed; dropped them (the events \
                 file replay re-sends them): {failure}",
                batch.len()
            );
            self.abandoned += batch.len() as u64;
            return;
        }
    }

    fn post(&mut self, batch: &[Value]) -> Sent {
        let sender = match service::sender(&mut self.sender) {
            Ok(sender) => sender,
            Err(error) => return Sent::Failed(error),
        };
        let body = serde_json::json!({"events": batch}).to_string();
        let request = (sender.client().post(&self.url))
            .timeout(LIMIT)
            .bearer_auth(&self.token)
            .header("Content-Type", "application/json")
            .body(body);
        let answer = match sender.send(request) {
            Ok(answer) => answer,
            Err(_) => return Sent::Again(None),
        };
        let detail = || {
            let body: String = answer.body.split_whitespace().collect::<Vec<_>>().join(" ");
            format!(
                "HTTP {}: {}",
                answer.status,
                body.chars().take(200).collect::<String>()
            )
        };
        match answer.status {
            200..=299 => {
                let refused: Value = serde_json::from_str(&answer.body).unwrap_or_default();
                for refusal in refused["refused"].as_array().into_iter().flatten() {
                    let event = refusal["index"]
                        .as_u64()
                        .and_then(|index| batch.get(index as usize));
                    let field =
                        |name: &str| event.map(|event| text(&event[name])).unwrap_or_default();
                    tracing::warn!(
                        "biffix ingest POST to {} refused one {} event ({}), dropped: {}",
                        service::safe_url(&self.url),
                        field("kind"),
                        field("task_key"),
                        text(&refusal["reason"])
                    );
                }
                Sent::Done
            }
            401 => Sent::Denied(detail()),
            400 | 413 => Sent::Rejected(detail()),
            408 | 429 | 500..=599 => {
                let wait = (answer.headers.get("Retry-After"))
                    .and_then(|value| value.to_str().ok()?.trim().parse::<u64>().ok())
                    .map(|seconds| Duration::from_secs(seconds).min(RETRY_MOST));
                Sent::Again(wait)
            }
            _ => Sent::Failed(detail()),
        }
    }
}

fn text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}
