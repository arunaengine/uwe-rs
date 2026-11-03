//! Builds stable run, task and attempt spans with recorded attributes and events.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::VecDeque;

use run_database::{
    Attempt, Database, DatabaseError, Event, MarkerRow, Run, TaskRow, redact_public_text,
};
use sha2::{Digest, Sha256};

use crate::otlp::{
    ERROR, INTERNAL, KeyValue, OK, Request, Resource, ResourceSpans, Scope, ScopeSpans, Span,
    SpanEvent, Status, UNSET,
};

/// The name of uwe as the service and the scope of its spans.
const SERVICE: &str = "uwe";

/// How many tasks are read from the run database at a time.
const TASK_PAGE: u32 = 256;

/// The spans of a run, in export requests of at most `per_request` spans each; `None` for an
/// unknown run. The run span comes first, then each task, followed by its attempts. This holds
/// the whole trace in memory: [`stream`] bounds it by the size of a request.
pub fn requests(
    database: &Database,
    run: &str,
    per_request: usize,
) -> Result<Option<Vec<Request>>, DatabaseError> {
    let mut all = Vec::new();
    let found = stream::<DatabaseError>(database, run, per_request, |request| {
        all.push(request);
        Ok(())
    })?;
    Ok(found.map(|_| all))
}

/// Gives each export request of a run to `each` as soon as it is built, in the order and with
/// the sizes of [`requests`], and returns the number of spans; `None` for an unknown run.
/// Tasks are read a page at a time, so memory is bounded by a page and a request, not the run.
pub fn stream<E: From<DatabaseError>>(
    database: &Database,
    run: &str,
    per_request: usize,
    each: impl FnMut(Request) -> Result<(), E>,
) -> Result<Option<usize>, E> {
    stream_pages(database, run, per_request, TASK_PAGE, each)
}

fn stream_pages<E: From<DatabaseError>>(
    database: &Database,
    run: &str,
    per_request: usize,
    page: u32,
    each: impl FnMut(Request) -> Result<(), E>,
) -> Result<Option<usize>, E> {
    let Some(record) = database.run(run)? else {
        return Ok(None);
    };
    let latest = database.last_event_time(run)?;
    let ended = record.ended.or(latest).unwrap_or(record.started);
    let ids = Ids::of(run);
    let tasks = record.tasks.values().sum::<u64>() as usize;
    let events = database.span_events(run, None)?;
    let mut batch = Batch::new(per_request, each);
    batch.push(run_span(&record, &ids, ended, tasks, &events))?;
    let mut after = 0;
    loop {
        let (rows, next) = database.task_page(run, after, page)?;
        if rows.is_empty() {
            break;
        }
        after = next;
        for task in &rows {
            let attempts = database.task_attempts(run, task.id)?;
            let events = database.span_events(run, Some(task.id))?;
            let markers = database.span_markers(run, Some(task.id))?;
            for span in task_spans(task, &attempts, &ids, ended, &events, markers.into()) {
                batch.push(span)?;
            }
        }
    }
    batch.finish().map(Some)
}

/// Spans that wait for the rest of their request.
struct Batch<F> {
    spans: Vec<Span>,
    size: usize,
    count: usize,
    each: F,
}

impl<E, F: FnMut(Request) -> Result<(), E>> Batch<F> {
    fn new(size: usize, each: F) -> Self {
        Batch {
            spans: Vec::new(),
            size: size.max(1),
            count: 0,
            each,
        }
    }

    /// Adds a span, and hands over the request when it is full.
    fn push(&mut self, span: Span) -> Result<(), E> {
        self.spans.push(span);
        if self.spans.len() >= self.size {
            self.send()?;
        }
        Ok(())
    }

    /// Hands over the last request and gives the number of spans of all requests.
    fn finish(mut self) -> Result<usize, E> {
        self.send()?;
        Ok(self.count)
    }

    fn send(&mut self) -> Result<(), E> {
        if self.spans.is_empty() {
            return Ok(());
        }
        let spans = std::mem::take(&mut self.spans);
        self.count += spans.len();
        (self.each)(request(spans))
    }
}

/// The span of a task followed by the spans of its attempts, each with its own events.
fn task_spans(
    task: &TaskRow,
    attempts: &[Attempt],
    ids: &Ids,
    ended: i64,
    events: &[Event],
    mut markers: VecDeque<MarkerRow>,
) -> Vec<Span> {
    let own = events
        .iter()
        .filter(|event| event.attempt.is_none())
        .collect::<Vec<_>>();
    let mut spans = vec![task_span(task, attempts, ids, ended, &own, &mut markers)];
    for attempt in attempts {
        let events = events
            .iter()
            .filter(|event| event.attempt == Some(attempt.number))
            .collect::<Vec<_>>();
        spans.push(attempt_span(task, attempt, ids, ended, &events));
    }
    spans
}

/// One export request with these spans of uwe.
pub(crate) fn request(spans: Vec<Span>) -> Request {
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

pub(crate) fn finished_spans(
    database: &Database,
    run: &str,
    task: u64,
    finalizing: bool,
) -> Result<Vec<Span>, DatabaseError> {
    let Some(detail) = database.task(run, task)? else {
        return Ok(Vec::new());
    };
    let row = &detail.row;
    if !finalizing
        && (!matches!(row.state.as_str(), "succeeded" | "failed")
            || !detail
                .attempts
                .last()
                .is_some_and(|attempt| attempt.accepted == Some(true)))
    {
        return Ok(Vec::new());
    }
    let ids = Ids::of(run);
    let events = database.span_events(run, Some(task))?;
    let ended = if finalizing {
        database.run(run)?.and_then(|record| record.ended)
    } else {
        None
    }
    .or_else(|| events.iter().map(|event| event.time).max())
    .unwrap_or(row.requested);
    let markers = database.span_markers(run, Some(task))?.into();
    Ok(task_spans(
        row,
        &detail.attempts,
        &ids,
        ended,
        &events,
        markers,
    ))
}

pub(crate) fn finished_run(database: &Database, run: &str) -> Result<Option<Span>, DatabaseError> {
    let Some(record) = database.run(run)? else {
        return Ok(None);
    };
    let Some(ended) = record.ended else {
        return Ok(None);
    };
    let tasks = record.tasks.values().sum::<u64>() as usize;
    let events = database.span_events(run, None)?;
    Ok(Some(run_span(
        &record,
        &Ids::of(run),
        ended,
        tasks,
        &events,
    )))
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
            trace: hex(&format!("uwe trace {run}"), 16),
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
        KeyValue::text("uwe.run.id", &run.id),
        KeyValue::text("uwe.run.lineage", &run.lineage),
        KeyValue::text("uwe.run.state", &run.state),
        KeyValue::text("uwe.workflow", &run.workflow),
        KeyValue::text("uwe.language", &run.language),
        KeyValue::text("uwe.executor", &run.executor),
        KeyValue::flag("uwe.simulated", run.simulated),
        KeyValue::integer("uwe.run.tasks", tasks as i64),
    ];
    let optional = [
        ("uwe.run.resumes", &run.resumes),
        ("uwe.run.replays", &run.replays),
        ("uwe.revision", &run.revision),
        ("uwe.store", &run.store),
        ("uwe.run.error", &run.error),
    ];
    for (key, value) in optional {
        if let Some(value) = value {
            attributes.push(KeyValue::text(key, value));
        }
    }
    for (name, value) in &run.parameters {
        attributes.push(KeyValue::text(&format!("uwe.parameter.{name}"), value));
    }
    let own = events.iter().filter(|event| event.task.is_none());
    Span {
        trace_id: ids.trace.clone(),
        span_id: ids.run_span(),
        parent_span_id: String::new(),
        name: format!("run {name}"),
        kind: INTERNAL,
        start_nanoseconds: nanoseconds(run.started),
        end_nanoseconds: nanoseconds(ended.max(run.started)),
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
        KeyValue::integer("uwe.task.id", task.id as i64),
        KeyValue::text("uwe.task.name", &task.name),
        KeyValue::text("uwe.task.step", &task.step),
        KeyValue::text("uwe.task.state", &task.state),
        KeyValue::integer("uwe.task.attempts", i64::from(task.attempts)),
    ];
    if let Some(code) = task.exit_code {
        attributes.push(KeyValue::integer("uwe.task.exit_code", i64::from(code)));
    }
    if let Some(reused) = &task.reused {
        attributes.push(KeyValue::text("uwe.task.reused", reused));
    }
    // Each `marked` event of the task goes with its next marker, in the order they were found.
    let events = (events.iter())
        .map(|found| {
            let marker = (found.kind == "marked")
                .then(|| markers.pop_front())
                .flatten();
            let attributes = marker.map_or_else(Vec::new, |marker| {
                vec![
                    KeyValue::text("uwe.marker.kind", &marker.kind),
                    KeyValue::text("uwe.marker.reason", &marker.reason),
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
        start_nanoseconds: nanoseconds(task.requested),
        end_nanoseconds: nanoseconds(end.max(task.requested)),
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
        KeyValue::integer("uwe.attempt.number", i64::from(attempt.number)),
        KeyValue::text("uwe.task.name", &task.name),
        KeyValue::flag("uwe.attempt.retried", attempt.retried),
    ];
    let texts = [
        ("uwe.attempt.image", &attempt.image),
        ("uwe.attempt.reused", &attempt.reused),
    ];
    for (key, value) in texts {
        if let Some(value) = value {
            attributes.push(KeyValue::text(key, value));
        }
    }
    let numbers = [
        ("uwe.attempt.exit_code", attempt.exit_code.map(i64::from)),
        ("uwe.attempt.cpu_usec", attempt.cpu_usec),
        ("uwe.attempt.memory_peak", attempt.memory_peak),
        ("uwe.attempt.read_bytes", attempt.read_bytes),
        ("uwe.attempt.write_bytes", attempt.write_bytes),
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
        start_nanoseconds: nanoseconds(attempt.started),
        end_nanoseconds: nanoseconds(end),
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
    fn awaits_output_validation() {
        use run_database::RunStart;
        use workflow_graph::{Language, Outcome, Task, TaskId};
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runs.sqlite");
        let start = RunStart {
            id: "validation".into(),
            resumes: None,
            workflow: "main.nf".into(),
            language: Language::Nextflow,
            revision: None,
            arguments: Vec::new(),
            parameters: Vec::new(),
            executor: "local".into(),
            simulated: false,
            store: None,
            program: None,
            launch: None,
            workflow_folder: None,
            repeats: None,
        };
        let mut recorder = Database::open(&path).unwrap().record(&start).unwrap();
        let committed = recorder.commits();
        let task = Task {
            id: TaskId(1),
            ..Task::default()
        };
        recorder.requested(&task, &[]);
        recorder.started(task.id, 1);
        recorder.finished(
            &Outcome {
                id: task.id,
                ..Outcome::default()
            },
            1,
            false,
        );
        committed
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        let database = Database::open(&path).unwrap();
        assert!(
            database
                .snapshot(|database| finished_spans(database, "validation", 1, false))
                .unwrap()
                .is_empty()
        );
        recorder.validated(task.id, 1, false);
        committed
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(
            database
                .snapshot(|database| finished_spans(database, "validation", 1, false))
                .unwrap()
                .is_empty()
        );
        let spans = database
            .snapshot(|database| finished_spans(database, "validation", 1, true))
            .unwrap();
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].status.code, ERROR);
        assert!(
            spans[1]
                .events
                .iter()
                .any(|event| event.name == "validated")
        );
        assert!(finished_run(&database, "validation").unwrap().is_none());
        recorder.requested(&task, &[]);
        recorder.started(task.id, 2);
        recorder.finished(
            &Outcome {
                id: task.id,
                ..Outcome::default()
            },
            2,
            false,
        );
        recorder.validated(task.id, 2, true);
        committed
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        let spans = database
            .snapshot(|database| finished_spans(database, "validation", 1, false))
            .unwrap();
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].status.code, OK);
        assert_eq!(spans[1].status.code, OK);
        assert_eq!(spans[2].status.code, OK);
        recorder.ended(None);
        recorder.close().unwrap();
    }

    #[test]
    fn streams_task_pages() {
        use run_database::RunStart;
        use workflow_graph::{Language, Outcome, Task, TaskId};
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runs.sqlite");
        let start = RunStart {
            id: "many".into(),
            resumes: None,
            workflow: "main.nf".into(),
            language: Language::Nextflow,
            revision: None,
            arguments: Vec::new(),
            parameters: Vec::new(),
            executor: "local".into(),
            simulated: false,
            store: None,
            program: None,
            launch: None,
            workflow_folder: None,
            repeats: None,
        };
        let mut recorder = Database::open(&path).unwrap().record(&start).unwrap();
        for id in 1..=25 {
            let task = Task {
                id: TaskId(id),
                step: format!("STEP{id}"),
                ..Task::default()
            };
            recorder.requested(&task, &[]);
            recorder.started(task.id, 1);
            let outcome = Outcome {
                id: task.id,
                ..Outcome::default()
            };
            recorder.finished(&outcome, 1, false);
        }
        recorder.ended(None);
        recorder.close().unwrap();
        let database = Database::open(&path).unwrap();
        let collect = |page: u32| {
            let mut requests = Vec::new();
            let count = stream_pages::<DatabaseError>(&database, "many", 10, page, |request| {
                requests.push(request);
                Ok(())
            })
            .unwrap();
            (count, requests)
        };
        let (count, paged) = collect(4);
        assert_eq!(count, Some(51), "the run and 25 tasks with an attempt each");
        let sizes: Vec<usize> = paged
            .iter()
            .map(|request| request.spans().count())
            .collect();
        assert_eq!(sizes, [10, 10, 10, 10, 10, 1]);
        let names: Vec<&str> = (paged.iter().flat_map(|request| request.spans()))
            .map(|span| span.name.as_str())
            .collect();
        assert_eq!(
            names[..5],
            ["run main.nf", "STEP1", "attempt 1", "STEP2", "attempt 1"]
        );
        assert_eq!(names[49..], ["STEP25", "attempt 1"]);
        let (_, single) = collect(1000);
        assert_eq!(
            serde_json::to_value(&paged).unwrap(),
            serde_json::to_value(&single).unwrap(),
            "the page size must not change the trace"
        );
        assert_eq!(
            requests(&database, "many", 10).unwrap().unwrap().len(),
            paged.len()
        );
        assert!(
            stream_pages::<DatabaseError>(&database, "none", 10, 4, |_| Ok(()))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn ids_stay_stable() {
        let ids = Ids::of("1a0f-2b");
        assert_eq!(ids.trace.len(), 32);
        assert_eq!(ids.run_span().len(), 16);
        assert_eq!(ids.task_span(3), Ids::of("1a0f-2b").task_span(3));
        assert_ne!(ids.task_span(3), ids.attempt_span(3, 1));
        assert!(ids.trace.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn encodes_nanosecond_text() {
        assert_eq!(nanoseconds(1_700_000_000_123), "1700000000123000000");
        assert_eq!(nanoseconds(-5), "0");
    }
}
