//! Streams changed run tasks and completion events through Server-Sent Events.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;
use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;
use run_database::{Database, MarkerRow, Run, TaskRow};
use serde::{Deserialize, Serialize};

use crate::{Failure, Shared, query};

/// How often the stream looks for new events.
const POLL: Duration = Duration::from_millis(500);

/// The most events one update covers; the next update follows at once.
const BATCH: u32 = 10_000;

#[derive(Deserialize)]
pub(crate) struct After {
    /// The number of the last event the page has.
    #[serde(default)]
    after: i64,
}

/// What changed in a run since the last update.
#[derive(Serialize)]
pub(crate) struct Update {
    /// The number of the last event this update covers.
    sequence: i64,
    run: Run,
    /// The tasks that were asked for, started or finished since the last update, and at the
    /// end of the run the tasks it stopped.
    tasks: Vec<TaskRow>,
    /// All markers of the run, when a new one came.
    markers: Option<Vec<MarkerRow>>,
    /// Whether the shape of the graph changed: a task was asked for, or a marker came. The
    /// page changes the states of the tasks in place, and draws the graph again only then.
    graph: bool,
    /// True only when all recorded updates of the terminal run have been sent.
    complete: bool,
}

pub(crate) async fn events(
    State(state): State<Shared>,
    Path(run): Path<String>,
    Query(After { after }): Query<After>,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, Failure> {
    state.authorize(&run)?;
    // A browser that reconnects says which event it saw last.
    let seen = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .unwrap_or(after);
    let start = Cursor {
        state,
        run,
        after: seen.max(after),
        ended: false,
        wait: false,
    };
    let stream = futures_util::stream::unfold(start, next);
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

struct Cursor {
    state: Shared,
    run: String,
    after: i64,
    /// Whether the last update showed the run as ended.
    ended: bool,
    /// Whether to wait before looking again: the last look found every event there was.
    wait: bool,
}

/// The next update of the stream, waiting until there is one; `None` ends the stream.
async fn next(mut cursor: Cursor) -> Option<(Result<Event, Infallible>, Cursor)> {
    if cursor.ended || cursor.state.stop.is_cancelled() {
        return None;
    }
    loop {
        if cursor.wait {
            tokio::select! {
                () = tokio::time::sleep(POLL) => {}
                () = cursor.state.stop.cancelled() => return None,
            }
        }
        let (run, after) = (cursor.run.clone(), cursor.after);
        let found = tokio::select! {
            found = query(&cursor.state, move |database| update(database, &run, after)) => found,
            () = cursor.state.stop.cancelled() => return None,
        };
        let (update, full) = match found {
            Ok(Some(found)) => found,
            // The run is gone, or the database cannot be read: end the stream.
            Ok(None) | Err(_) => return None,
        };
        cursor.wait = !full;
        let running = update.run.state == "running";
        if update.sequence == cursor.after && running {
            continue;
        }
        cursor.after = update.sequence;
        cursor.ended = update.complete;
        let event = Event::default()
            .event("update")
            .id(update.sequence.to_string())
            .json_data(run_database::public_json(&update).unwrap_or_default())
            .unwrap_or_default();
        return Some((Ok(event), cursor));
    }
}

/// The update after an event, and whether it covers a full batch; `None` for an unknown run.
fn update(database: &Database, run: &str, after: i64) -> Result<Option<(Update, bool)>, Failure> {
    // A terminal state must be observed before reading its final event backlog.
    let Some(state) = database.run(run)? else {
        return Ok(None);
    };
    let events = database.events(run, after, BATCH)?;
    let touched: BTreeSet<u64> = events.iter().filter_map(|event| event.task).collect();
    let mut tasks = Vec::with_capacity(touched.len());
    for task in &touched {
        tasks.extend(database.task_row(run, *task)?);
    }
    // At its end a run stops the tasks that had not finished, without an event for each.
    if events.iter().any(|event| event.kind == "ended") {
        let stopped = database.tasks(run)?.into_iter();
        tasks.extend(stopped.filter(|task| task.state == "stopped" && !touched.contains(&task.id)));
    }
    let marked = events.iter().any(|event| event.kind == "marked");
    let graph = events
        .iter()
        .any(|event| matches!(event.kind.as_str(), "requested" | "marked"));
    let full = events.len() as u32 == BATCH;
    let complete = state.state != "running" && !full;
    let update = Update {
        sequence: events.last().map_or(after, |event| event.sequence),
        run: state,
        tasks,
        markers: if marked {
            Some(database.markers(run)?)
        } else {
            None
        },
        graph,
        complete,
    };
    Ok(Some((update, full)))
}
