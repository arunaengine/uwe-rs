//! The routes of the page: the page itself, and the JSON that its script reads.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use run_database::{Log, MarkerRow, Run, TaskDetail, TaskRow, Why};
use serde::{Deserialize, Serialize};
use workflow_graph::render::{Format, Options, View, render};

use crate::{Failure, Shared, assets, files, live, query};

/// How many runs the list of runs shows.
const RUNS: u32 = 200;

pub(crate) fn router(state: Shared) -> Router {
    Router::new()
        .route("/", get(assets::page))
        .route("/runs/{run}", get(assets::page))
        .route("/runs/{run}/tasks/{task}", get(assets::page))
        .route("/page.css", get(assets::style))
        .route("/page.js", get(assets::script))
        .route("/run.js", get(assets::run_script))
        .route("/api/runs", get(runs))
        .route("/api/runs/{run}", get(run))
        .route("/api/runs/{run}/events", get(live::events))
        .route("/api/runs/{run}/graph.svg", get(graph))
        .route("/api/runs/{run}/tasks/{task}", get(task))
        .route("/api/runs/{run}/tasks/{task}/why", get(why))
        .route(
            "/api/runs/{run}/tasks/{task}/logs/{attempt}/{stream}",
            get(files::log),
        )
        .route(
            "/api/runs/{run}/tasks/{task}/{role}/{index}",
            get(files::file),
        )
        .with_state(state)
}

async fn runs(State(state): State<Shared>) -> Result<Json<Vec<Run>>, Failure> {
    Ok(Json(
        query(&state, |database| Ok(database.runs(RUNS)?)).await?,
    ))
}

/// A run with all its tasks, for the first view of the page. Later changes come as events.
#[derive(Serialize)]
struct RunView {
    run: Run,
    tasks: Vec<TaskRow>,
    markers: Vec<MarkerRow>,
    /// The number of the last event that this view holds.
    sequence: i64,
}

async fn run(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> Result<Json<RunView>, Failure> {
    let view = query(&state, move |database| {
        // The sequence first: an event that comes while the rest is read is sent again.
        let sequence = database.last_event(&id)?;
        let Some(run) = database.run(&id)? else {
            return Err(Failure::not_found(format!("no run `{id}`")));
        };
        Ok(RunView {
            tasks: database.tasks(&id)?,
            markers: database.markers(&id)?,
            run,
            sequence,
        })
    })
    .await?;
    Ok(Json(view))
}

/// A task with its logs, and for each input that an earlier task of the run made, that task.
#[derive(Serialize)]
struct TaskView {
    #[serde(flatten)]
    detail: TaskDetail,
    stdout: Option<Log>,
    stderr: Option<Log>,
    /// The task that made each input, by the input's location.
    producers: BTreeMap<String, u64>,
}

async fn task(
    State(state): State<Shared>,
    Path((id, task)): Path<(String, u64)>,
) -> Result<Json<TaskView>, Failure> {
    let view = query(&state, move |database| {
        let Some(detail) = database.task(&id, task)? else {
            return Err(Failure::not_found(format!("no task {task} in run `{id}`")));
        };
        let attempt = detail.attempts.last().map(|attempt| attempt.number);
        let log = |stream| match attempt {
            Some(attempt) => database.log(&id, task, attempt, stream),
            None => Ok(None),
        };
        let (stdout, stderr) = (log("stdout")?, log("stderr")?);
        let mut producers = BTreeMap::new();
        for input in detail
            .inputs
            .iter()
            .filter(|input| !input.location.is_empty())
        {
            let made = database.producers(&input.location)?;
            if let Some((_, file)) = made.iter().find(|(run, _)| run == &id) {
                producers.insert(input.location.clone(), file.task);
            }
        }
        Ok(TaskView {
            detail,
            stdout,
            stderr,
            producers,
        })
    })
    .await?;
    Ok(Json(view))
}

/// Why a task ran instead of taking the result of an earlier run; `null` without an earlier run.
async fn why(
    State(state): State<Shared>,
    Path((id, task)): Path<(String, u64)>,
) -> Result<Json<Option<Why>>, Failure> {
    let why = query(&state, move |database| Ok(database.why(&id, task)?)).await?;
    Ok(Json(why))
}

#[derive(Deserialize)]
struct GraphView {
    /// `tasks` or `steps`; `tasks` by default.
    view: Option<String>,
}

/// The graph of a run as SVG, drawn by the same renderer as `iunctus graph`: one box per task
/// or one per step, with the states of the tasks and the markers of a simulated run.
async fn graph(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Query(GraphView { view }): Query<GraphView>,
) -> Result<impl IntoResponse, Failure> {
    let view = match view.as_deref() {
        None => View::Tasks,
        Some(name) => View::parse(name)
            .ok_or_else(|| Failure::bad_request("the view of a graph is `tasks` or `steps`"))?,
    };
    let svg = query(&state, move |database| {
        let Some(graph) = database.graph(&id)? else {
            return Err(Failure::not_found(format!("no run `{id}`")));
        };
        let options = Options {
            view,
            ..Options::default()
        };
        Ok(render(&graph, &options, Format::Svg))
    })
    .await?;
    Ok(([(header::CONTENT_TYPE, "image/svg+xml")], svg))
}
