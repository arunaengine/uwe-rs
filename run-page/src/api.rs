//! The routes of the page: the page itself, and the JSON that its script reads.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap};

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use run_database::{
    Comparison, Database, DatabaseError, Log, MarkerRow, Run, TaskDetail, TaskRow, TimeSpent, Why,
};
use serde::{Deserialize, Serialize};
use workflow_graph::TaskId;
use workflow_graph::render::{Direction, FOLD, Format, Options, State as Shown, View, render};

use crate::{Failure, Shared, assets, files, live, query};

/// How many runs the list of runs shows.
const RUNS: u32 = 200;

pub(crate) fn router(state: Shared) -> Router {
    Router::new()
        .route("/", get(assets::page))
        .route("/runs/{run}", get(assets::page))
        .route("/runs/{run}/tasks/{task}", get(assets::page))
        .route("/compare/{first}/{second}", get(assets::page))
        .route("/page.css", get(assets::style))
        .route("/page.js", get(assets::script))
        .route("/run.js", get(assets::run_script))
        .route("/graph.js", get(assets::graph_script))
        .route("/camera.js", get(assets::camera_script))
        .route("/boxes.js", get(assets::boxes_script))
        .route("/time.js", get(assets::time_script))
        .route("/compare.js", get(assets::compare_script))
        .route("/api/runs", get(runs))
        .route("/api/runs/{run}", get(run))
        .route("/api/runs/{run}/events", get(live::events))
        .route("/api/runs/{run}/graph.svg", get(graph))
        .route("/api/runs/{run}/time", get(time))
        .route("/api/compare/{first}/{second}", get(compare))
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
pub(crate) struct RunView {
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
        run_view(database, &id)?.ok_or_else(|| Failure::not_found(format!("no run `{id}`")))
    })
    .await?;
    Ok(Json(view))
}

/// The first view of a run, or `None` when the database has no such run.
pub(crate) fn run_view(database: &Database, id: &str) -> Result<Option<RunView>, DatabaseError> {
    // The sequence first: an event that comes while the rest is read is sent again.
    let sequence = database.last_event(id)?;
    let Some(run) = database.run(id)? else {
        return Ok(None);
    };
    Ok(Some(RunView {
        tasks: database.tasks(id)?,
        markers: database.markers(id)?,
        run,
        sequence,
    }))
}

/// A task with its logs, and for each input that an earlier task of the run made, that task.
#[derive(Serialize)]
pub(crate) struct TaskView {
    #[serde(flatten)]
    detail: TaskDetail,
    pub(crate) stdout: Option<Log>,
    pub(crate) stderr: Option<Log>,
    /// The task that made each input, by the input's location.
    producers: BTreeMap<String, u64>,
    /// The runs that replay the task, the latest first.
    replays: Vec<Run>,
}

async fn task(
    State(state): State<Shared>,
    Path((id, task)): Path<(String, u64)>,
) -> Result<Json<TaskView>, Failure> {
    let view = query(&state, move |database| {
        task_view(database, &id, task)?
            .ok_or_else(|| Failure::not_found(format!("no task {task} in run `{id}`")))
    })
    .await?;
    Ok(Json(view))
}

/// A task with its logs, or `None` when the run has no such task.
pub(crate) fn task_view(
    database: &Database,
    id: &str,
    task: u64,
) -> Result<Option<TaskView>, DatabaseError> {
    let Some(detail) = database.task(id, task)? else {
        return Ok(None);
    };
    let attempt = detail.attempts.last().map(|attempt| attempt.number);
    let log = |stream| match attempt {
        Some(attempt) => database.log(id, task, attempt, stream),
        None => Ok(None),
    };
    let (stdout, stderr) = (log("stdout")?, log("stderr")?);
    let mut producers = BTreeMap::new();
    for input in (detail.inputs.iter()).filter(|input| !input.location.is_empty()) {
        let made = database.producers(&input.location)?;
        if let Some((_, file)) = made.iter().find(|(run, _)| run == id) {
            producers.insert(input.location.clone(), file.task);
        }
    }
    Ok(Some(TaskView {
        replays: database.replays_of(id, task)?,
        detail,
        stdout,
        stderr,
        producers,
    }))
}

/// Why a task ran instead of taking the result of an earlier run; `null` without an earlier run.
async fn why(
    State(state): State<Shared>,
    Path((id, task)): Path<(String, u64)>,
) -> Result<Json<Option<Why>>, Failure> {
    let why = query(&state, move |database| Ok(database.why(&id, task)?)).await?;
    Ok(Json(why))
}

/// Where the time of a run went: the critical path, waiting and running, and the steps.
async fn time(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> Result<Json<TimeSpent>, Failure> {
    let spent = query(&state, move |database| match database.time_spent(&id)? {
        Some(spent) => Ok(spent),
        None => Err(Failure::not_found(format!("no run `{id}`"))),
    })
    .await?;
    Ok(Json(spent))
}

/// What differs between two runs, task by task.
async fn compare(
    State(state): State<Shared>,
    Path((first, second)): Path<(String, String)>,
) -> Result<Json<Comparison>, Failure> {
    let comparison = query(&state, move |database| {
        match database.compare(&first, &second)? {
            Some(comparison) => Ok(comparison),
            None => Err(Failure::not_found(format!(
                "no run `{first}` or no run `{second}`"
            ))),
        }
    })
    .await?;
    Ok(Json(comparison))
}

#[derive(Deserialize)]
struct GraphView {
    /// `tasks` or `steps`; `tasks` by default.
    view: Option<String>,
    /// `down` or `right`; `down` by default.
    direction: Option<String>,
    /// The steps whose tasks are drawn although they have many, one name per line.
    open: Option<String>,
    /// A step with more tasks than this is one box; 0 draws every task.
    fold: Option<usize>,
}

/// The graph of a run as SVG, drawn by the same renderer as `iunctus graph`: one box per task
/// with large steps folded, or one per step, down or to the right, with the states of the
/// tasks as the database knows them and the markers of a simulated run.
async fn graph(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Query(asked): Query<GraphView>,
) -> Result<impl IntoResponse, Failure> {
    let view = match asked.view.as_deref() {
        None => View::Tasks,
        Some(name) => View::parse(name)
            .ok_or_else(|| Failure::bad_request("the view of a graph is `tasks` or `steps`"))?,
    };
    let direction = match asked.direction.as_deref() {
        None => Direction::Down,
        Some(name) => Direction::parse(name)
            .ok_or_else(|| Failure::bad_request("the direction of a graph is `down` or `right`"))?,
    };
    let open = asked.open.unwrap_or_default();
    let options = Options {
        view,
        direction,
        fold: asked.fold.unwrap_or(FOLD),
        open: open.lines().map(str::to_owned).collect(),
        states: HashMap::new(),
    };
    let svg = query(&state, move |database| {
        picture(database, &id, options)?.ok_or_else(|| Failure::not_found(format!("no run `{id}`")))
    })
    .await?;
    Ok(([(header::CONTENT_TYPE, "image/svg+xml")], svg))
}

/// The picture of the graph of a run with the states of its tasks, or `None` when the database
/// has no such run.
pub(crate) fn picture(
    database: &Database,
    id: &str,
    mut options: Options,
) -> Result<Option<String>, DatabaseError> {
    let Some(graph) = database.graph(id)? else {
        return Ok(None);
    };
    let simulated = graph.is_simulated();
    options.states = (database.tasks(id)?.iter())
        .map(|row| (TaskId(row.id), shown(row, simulated)))
        .collect();
    Ok(Some(render(&graph, &options, Format::Svg)))
}

/// The state that the graph shows for a task: waiting and running come from the database,
/// which knows them while the run goes on.
fn shown(row: &TaskRow, simulated: bool) -> Shown {
    match row.state.as_str() {
        "succeeded" if row.reused.is_some() => Shown::Reused,
        "succeeded" if simulated => Shown::Simulated,
        "succeeded" => Shown::Succeeded,
        "failed" => Shown::Failed,
        "running" => Shown::Running,
        "stopped" => Shown::Stopped,
        _ => Shown::Waiting,
    }
}
