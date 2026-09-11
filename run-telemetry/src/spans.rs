//! The spans of a run from the run database: the run as the root span, each task as a span
//! from the moment the workflow asked for it until its last attempt ended, and each attempt as
//! a span below its task. Their attributes come from the database; the events of the engine
//! (`requested`, `started`, `finished`, `reused`, `marked`, `ended`) become events of the span
//! they concern, and each marker an event with its kind and reason. Ids are made from the run
//! id, so that the same run always gives the same trace.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, VecDeque};

use run_database::{
    Attempt, Database, DatabaseError, Event, MarkerRow, Run, TaskRow, redact_public_text,
};
use sha2::{Digest, Sha256};

use crate::otlp::{
    ERROR, INTERNAL, KeyValue, OK, Request, Resource, ResourceSpans, Scope, ScopeSpans, Span,
    SpanEvent, Status, UNSET,
};

/// The name of iunctus as the service and the scope of its spans.
const SERVICE: &str = "iunctus";

/// The spans of a run, in export requests of at most `per_request` spans each; `None` for an
/// unknown run. The run span comes first, then each task, followed by its attempts.
pub fn requests(
    database: &Database,
    run: &str,
    per_request: usize,
) -> Result<Option<Vec<Request>>, DatabaseError> {
    let Some(record) = database.run(run)? else {
        return Ok(None);
    };
    let tasks = database.tasks(run)?;
    let mut attempts: HashMap<u64, Vec<Attempt>> = HashMap::new();
    for (task, attempt) in database.attempts(run)? {
        attempts.entry(task).or_default().push(attempt);
    }
    let events = database.events(run, 0, u32::MAX)?;
    let mut markers: HashMap<u64, VecDeque<MarkerRow>> = HashMap::new();
    for marker in database.markers(run)? {
        markers.entry(marker.task).or_default().push_back(marker);
    }
    let latest = (events.iter().map(|event| event.time)).max();
    let ended = record.ended.or(latest).unwrap_or(record.started);
    let ids = Ids::of(run);
    let mut spans = vec![run_span(&record, &ids, ended, tasks.len(), &events)];
    let mut by_task: HashMap<(u64, Option<u32>), Vec<&Event>> = HashMap::new();
    for event in &events {
        if let Some(task) = event.task {
            by_task
                .entry((task, event.attempt))
                .or_default()
                .push(event);
        }
    }
    for task in &tasks {
        let tried = attempts.remove(&task.id).unwrap_or_default();
        let mut marked = markers.remove(&task.id).unwrap_or_default();
        let events = by_task.remove(&(task.id, None)).unwrap_or_default();
        spans.push(task_span(task, &tried, &ids, ended, &events, &mut marked));
        for attempt in &tried {
            let events = (by_task.remove(&(task.id, Some(attempt.number)))).unwrap_or_default();
            spans.push(attempt_span(task, attempt, &ids, ended, &events));
        }
    }
    let requests = spans
        .chunks(per_request.max(1))
        .map(|chunk| request(chunk.to_vec()))
        .collect();
    Ok(Some(requests))
}

/// One export request with these spans of iunctus.
fn request(spans: Vec<Span>) -> Request {
    let version = env!("CARGO_PKG_VERSION");
    Request {
        resource_spans: vec![ResourceSpans {
            resource: Resource {
                attributes: vec![
                    KeyValue::text("service.name", SERVICE),
                    KeyValue::text("service.version", version),
                ],
            },
            scope_spans: vec![ScopeSpans {
                scope: Scope {
                    name: SERVICE.to_owned(),
                    version: version.to_owned(),
                },
                spans: spans.into_iter().map(public_span).collect(),
            }],
        }],
    }
}

/// Redacts private text at the boundary of the public telemetry artifact.
fn public_span(mut span: Span) -> Span {
    span.name = redact_public_text(&span.name);
    redact_attributes(&mut span.attributes);
    for event in &mut span.events {
        event.name = redact_public_text(&event.name);
        redact_attributes(&mut event.attributes);
    }
    span.status.message = redact_public_text(&span.status.message);
    span
}

fn redact_attributes(attributes: &mut [KeyValue]) {
    for attribute in attributes {
        attribute.key = redact_public_text(&attribute.key);
        if let crate::otlp::Value::Text(text) = &mut attribute.value {
            *text = redact_public_text(text);
        }
    }
}

/// The ids of the spans of a run: the trace from the run id, and each span from the run id and
/// the task and attempt, so that the same run always gives the same ids.
struct Ids {
    run: String,
    trace: String,
}

impl Ids {
    fn of(run: &str) -> Ids {
        Ids {
            run: run.to_owned(),
            trace: hex(&format!("iunctus trace {run}"), 16),
        }
    }

    fn run_span(&self) -> String {
        hex(&format!("{}/run", self.run), 8)
    }

    fn task_span(&self, task: u64) -> String {
        hex(&format!("{}/task/{task}", self.run), 8)
    }

    fn attempt_span(&self, task: u64, attempt: u32) -> String {
        hex(&format!("{}/task/{task}/attempt/{attempt}", self.run), 8)
    }
}

/// The first `bytes` bytes of the SHA-256 of a text, in hex.
fn hex(text: &str, bytes: usize) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest[..bytes]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Milliseconds since the Unix epoch as nanoseconds, in the decimal text of OTLP JSON.
fn nanoseconds(milliseconds: i64) -> String {
    (i128::from(milliseconds.max(0)) * 1_000_000).to_string()
}

/// The status of a span by the state it ended in.
fn status(state: &str, message: Option<&str>) -> Status {
    let code = match state {
        "succeeded" => OK,
        "failed" | "stopped" => ERROR,
        _ => UNSET,
    };
    Status {
        code,
        message: if code == ERROR {
            message.unwrap_or(state).to_owned()
        } else {
            String::new()
        },
    }
}

fn event(event: &Event, attributes: Vec<KeyValue>) -> SpanEvent {
    SpanEvent {
        time_unix_nano: nanoseconds(event.time),
        name: event.kind.clone(),
        attributes,
    }
}

/// The run: its workflow, language, executor, parameters, and how it ended.
fn run_span(run: &Run, ids: &Ids, ended: i64, tasks: usize, events: &[Event]) -> Span {
    let workflow = std::path::Path::new(&run.workflow);
    let name = (workflow.file_name()).map_or_else(
        || run.workflow.clone(),
        |name| name.to_string_lossy().into_owned(),
    );
    let mut attributes = vec![
        KeyValue::text("iunctus.run.id", &run.id),
        KeyValue::text("iunctus.run.lineage", &run.lineage),
        KeyValue::text("iunctus.run.state", &run.state),
        KeyValue::text("iunctus.workflow", &run.workflow),
        KeyValue::text("iunctus.language", &run.language),
        KeyValue::text("iunctus.executor", &run.executor),
        KeyValue::flag("iunctus.simulated", run.simulated),
        KeyValue::integer("iunctus.run.tasks", tasks as i64),
    ];
    let optional = [
        ("iunctus.run.resumes", &run.resumes),
        ("iunctus.run.replays", &run.replays),
        ("iunctus.revision", &run.revision),
        ("iunctus.store", &run.store),
        ("iunctus.run.error", &run.error),
    ];
    for (key, value) in optional {
        if let Some(value) = value {
            attributes.push(KeyValue::text(key, value));
        }
    }
    for (name, value) in &run.parameters {
        attributes.push(KeyValue::text(&format!("iunctus.parameter.{name}"), value));
    }
    let own = events.iter().filter(|event| event.task.is_none());
    Span {
        trace_id: ids.trace.clone(),
        span_id: ids.run_span(),
        parent_span_id: String::new(),
        name: format!("run {name}"),
        kind: INTERNAL,
        start_time_unix_nano: nanoseconds(run.started),
        end_time_unix_nano: nanoseconds(ended.max(run.started)),
        attributes,
        events: own.map(|found| event(found, Vec::new())).collect(),
        status: status(&run.state, run.error.as_deref()),
    }
}

/// A task, from the moment the workflow asked for it until its last attempt ended, with its
/// markers as events.
fn task_span(
    task: &TaskRow,
    attempts: &[Attempt],
    ids: &Ids,
    ended: i64,
    events: &[&Event],
    markers: &mut VecDeque<MarkerRow>,
) -> Span {
    let end = match attempts.last() {
        Some(last) => last.ended.unwrap_or(ended),
        None => task.requested,
    };
    let mut attributes = vec![
        KeyValue::integer("iunctus.task.id", task.id as i64),
        KeyValue::text("iunctus.task.name", &task.name),
        KeyValue::text("iunctus.task.step", &task.step),
        KeyValue::text("iunctus.task.state", &task.state),
        KeyValue::integer("iunctus.task.attempts", i64::from(task.attempts)),
    ];
    if let Some(code) = task.exit_code {
        attributes.push(KeyValue::integer("iunctus.task.exit_code", i64::from(code)));
    }
    if let Some(reused) = &task.reused {
        attributes.push(KeyValue::text("iunctus.task.reused", reused));
    }
    // Each `marked` event of the task goes with its next marker, in the order they were found.
    let events = (events.iter())
        .map(|found| {
            let marker = (found.kind == "marked")
                .then(|| markers.pop_front())
                .flatten();
            let attributes = marker.map_or_else(Vec::new, |marker| {
                vec![
                    KeyValue::text("iunctus.marker.kind", &marker.kind),
                    KeyValue::text("iunctus.marker.reason", &marker.reason),
                ]
            });
            event(found, attributes)
        })
        .collect();
    let message = task.exit_code.map(|code| format!("exit code {code}"));
    Span {
        trace_id: ids.trace.clone(),
        span_id: ids.task_span(task.id),
        parent_span_id: ids.run_span(),
        name: task.step.clone(),
        kind: INTERNAL,
        start_time_unix_nano: nanoseconds(task.requested),
        end_time_unix_nano: nanoseconds(end.max(task.requested)),
        attributes,
        events,
        status: status(&task.state, message.as_deref()),
    }
}

/// An attempt of a task, with the image it ran in and what it used.
fn attempt_span(
    task: &TaskRow,
    attempt: &Attempt,
    ids: &Ids,
    ended: i64,
    events: &[&Event],
) -> Span {
    let mut attributes = vec![
        KeyValue::integer("iunctus.attempt.number", i64::from(attempt.number)),
        KeyValue::text("iunctus.task.name", &task.name),
        KeyValue::flag("iunctus.attempt.retried", attempt.retried),
    ];
    let texts = [
        ("iunctus.attempt.image", &attempt.image),
        ("iunctus.attempt.reused", &attempt.reused),
    ];
    for (key, value) in texts {
        if let Some(value) = value {
            attributes.push(KeyValue::text(key, value));
        }
    }
    let numbers = [
        (
            "iunctus.attempt.exit_code",
            attempt.exit_code.map(i64::from),
        ),
        ("iunctus.attempt.cpu_usec", attempt.cpu_usec),
        ("iunctus.attempt.memory_peak", attempt.memory_peak),
        ("iunctus.attempt.read_bytes", attempt.read_bytes),
        ("iunctus.attempt.write_bytes", attempt.write_bytes),
    ];
    for (key, value) in numbers {
        if let Some(value) = value {
            attributes.push(KeyValue::integer(key, value));
        }
    }
    let end = attempt.ended.unwrap_or(ended).max(attempt.started);
    let status = match attempt.exit_code {
        Some(0) => status("succeeded", None),
        Some(code) => status("failed", Some(&format!("exit code {code}"))),
        None if attempt.ended.is_none() => status("stopped", Some("the attempt did not end")),
        None => status("", None),
    };
    Span {
        trace_id: ids.trace.clone(),
        span_id: ids.attempt_span(task.id, attempt.number),
        parent_span_id: ids.task_span(task.id),
        name: format!("attempt {}", attempt.number),
        kind: INTERNAL,
        start_time_unix_nano: nanoseconds(attempt.started),
        end_time_unix_nano: nanoseconds(end),
        attributes,
        events: events
            .iter()
            .map(|found| event(found, Vec::new()))
            .collect(),
        status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_hex_of_their_size_and_stay_the_same() {
        let ids = Ids::of("1a0f-2b");
        assert_eq!(ids.trace.len(), 32);
        assert_eq!(ids.run_span().len(), 16);
        assert_eq!(ids.task_span(3), Ids::of("1a0f-2b").task_span(3));
        assert_ne!(ids.task_span(3), ids.attempt_span(3, 1));
        assert!(ids.trace.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn times_are_nanoseconds_as_text() {
        assert_eq!(nanoseconds(1_700_000_000_123), "1700000000123000000");
        assert_eq!(nanoseconds(-5), "0");
    }
}
