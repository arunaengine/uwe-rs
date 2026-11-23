//! The routes that describe workflows and start, stop and follow runs through the studio.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::convert::Infallible;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::Stream;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::api::public;
use crate::studio::{Capabilities, LaunchRequest, Phase, ResumeFrom, RunStarted, WorkflowEntry};
use crate::{Failure, Shared};

/// What the page may do: show the runs only, or start them too, with which executors.
#[derive(Serialize)]
pub(crate) struct Offered {
    studio: bool,
    #[serde(flatten)]
    capabilities: Capabilities,
}

pub(crate) async fn capabilities(State(state): State<Shared>) -> Json<Offered> {
    let studio = state.studio.as_deref();
    Json(Offered {
        studio: studio.is_some(),
        capabilities: studio
            .map(|studio| studio.capabilities())
            .unwrap_or_default(),
    })
}

/// The workflows of the launcher.
#[derive(Serialize)]
struct Listed {
    workflows: Vec<WorkflowEntry>,
}

pub(crate) async fn workflows(State(state): State<Shared>) -> Result<Json<Value>, Failure> {
    let workflows = state.studio()?.workflows().await?;
    public(&Listed { workflows })
}

pub(crate) async fn describe(
    State(state): State<Shared>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, Failure> {
    public(&state.studio()?.describe(typed(request)?).await?)
}

pub(crate) async fn validate(
    State(state): State<Shared>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, Failure> {
    public(&state.studio()?.validate(typed(request)?).await?)
}

/// Starts a run; it answers once the run is admitted, with `202 Accepted` and its id.
pub(crate) async fn start(
    State(state): State<Shared>,
    Json(request): Json<Value>,
) -> Result<Response, Failure> {
    let request = resumable(&state, typed(request)?)?;
    let started = state.studio()?.launch(request).await?;
    accepted(&state, &started)
}

/// Starts a run again with the workflow and the values of a recorded run.
pub(crate) async fn repeat(
    State(state): State<Shared>,
    Path(run): Path<String>,
    body: Option<Json<Value>>,
) -> Result<Response, Failure> {
    state.authorize(&run)?;
    let changes = match body {
        Some(Json(changes)) => typed(changes)?,
        None => LaunchRequest::default(),
    };
    let changes = resumable(&state, changes)?;
    let started = state.studio()?.repeat(run, changes).await?;
    accepted(&state, &started)
}

/// A request that a page for some runs may make: it resumes only one of them, and the studio
/// looks for the last run of a workflow among them. Any other page resumes any run.
pub(crate) fn resumable(
    state: &Shared,
    mut request: LaunchRequest,
) -> Result<LaunchRequest, Failure> {
    if let Some(Some(run)) = request.resume.as_ref().and_then(ResumeFrom::run) {
        state.authorize(run)?;
    }
    request.allowed = state.allowed();
    Ok(request)
}

/// A request body of its type; a body that does not fit is a bad request, as it says why.
pub(crate) fn typed<T: DeserializeOwned>(body: Value) -> Result<T, Failure> {
    serde_json::from_value(body).map_err(|error| Failure::bad_request(error.to_string()))
}

/// The answer for a run that the page started, which the page may show from now on.
pub(crate) fn accepted(state: &Shared, started: &RunStarted) -> Result<Response, Failure> {
    state.admit(&started.run_id);
    Ok((StatusCode::ACCEPTED, public(started)?).into_response())
}

/// What the page says of a run that it asked to stop.
#[derive(Serialize)]
struct Stopping {
    run_id: String,
    state: Phase,
}

/// Asks a run that the page started to stop. It answers at once; the run ends a little later.
pub(crate) async fn cancel(
    State(state): State<Shared>,
    Path(run): Path<String>,
) -> Result<Json<Value>, Failure> {
    state.authorize(&run)?;
    if !state.studio()?.cancel(&run) {
        let message = format!("run `{run}` does not go on in this page");
        return Err(Failure::conflict(message));
    }
    public(&Stopping {
        run_id: run,
        state: Phase::Canceling,
    })
}

/// The live output and resource use of a run as Server-Sent Events named by their `kind`, and
/// `end` when the run has ended or was not started by this page.
pub(crate) async fn stream(
    State(state): State<Shared>,
    Path(run): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, Failure> {
    state.authorize(&run)?;
    let follow = Follow {
        receiver: state.studio()?.follow(&run),
        stop: state.stop.clone(),
        ended: false,
    };
    let stream = futures_util::stream::unfold(follow, next);
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

struct Follow {
    receiver: Option<broadcast::Receiver<Value>>,
    stop: CancellationToken,
    ended: bool,
}

/// The next live event; `None` ends the stream after its `end` event or when the page stops.
async fn next(mut follow: Follow) -> Option<(Result<Event, Infallible>, Follow)> {
    if follow.ended {
        return None;
    }
    let Some(receiver) = follow.receiver.as_mut() else {
        follow.ended = true;
        return Some((Ok(Event::default().event("end").data("{}")), follow));
    };
    let received = tokio::select! {
        received = receiver.recv() => received,
        () = follow.stop.cancelled() => return None,
    };
    let event = match received {
        Ok(value) => {
            let kind = value.get("kind").and_then(Value::as_str).unwrap_or("log");
            let shown = run_database::public_json(&value).unwrap_or_default();
            (Event::default().event(kind).json_data(shown)).unwrap_or_default()
        }
        // A page that reads slowly misses events, and says how many.
        Err(broadcast::error::RecvError::Lagged(missed)) => {
            Event::default().event("lagged").data(missed.to_string())
        }
        Err(broadcast::error::RecvError::Closed) => {
            follow.ended = true;
            Event::default().event("end").data("{}")
        }
    };
    Some((Ok(event), follow))
}
