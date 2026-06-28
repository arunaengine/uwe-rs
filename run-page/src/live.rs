//! Live updates of a run as Server-Sent Events. The stream reads new events from the database
//! twice a second and sends one update with the changed tasks, so that a run of thousands of
//! short tasks sends few messages. It ends after the update that shows the run has ended.
// Copyright (c) 2026 The iunctus Contributors
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
    /// Whether the graph changed: a task was asked for or finished, or a marker came.
    graph: bool,
}

pub(crate) async fn events(
    State(state): State<Shared>,
    Path(run): Path<String>,
    Query(After { after }): Query<After>,
    headers: HeaderMap,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
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
    Sse::new(stream).keep_alive(KeepAlive::default())
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
    if cursor.ended {
        return None;
    }
    loop {
        if cursor.wait {
            tokio::time::sleep(POLL).await;
        }
        let (run, after) = (cursor.run.clone(), cursor.after);
        let found = query(&cursor.state, move |database| update(database, &run, after)).await;
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
        cursor.ended = !running;
        let event = Event::default()
            .event("update")
            .id(update.sequence.to_string())
            .json_data(&update)
            .unwrap_or_default();
        return Some((Ok(event), cursor));
    }
}

/// The update after an event, and whether it covers a full batch; `None` for an unknown run.
fn update(database: &Database, run: &str, after: i64) -> Result<Option<(Update, bool)>, Failure> {
    let events = database.events(run, after, BATCH)?;
    let Some(state) = database.run(run)? else {
        return Ok(None);
    };
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
        .any(|event| matches!(event.kind.as_str(), "requested" | "finished" | "marked"));
    let full = events.len() as u32 == BATCH;
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
    };
    Ok(Some((update, full)))
}
