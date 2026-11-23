//! The routes of the task explorer and the combiner: the tool library and composed workflows.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::Value;

use crate::api::public;
use crate::launch::{accepted, typed};
use crate::studio::ToolFilter;
use crate::{Failure, Shared};

pub(crate) fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/tools", get(tools))
        .route("/api/tools/scan", post(scan))
        .route("/api/tools/suggest", post(suggest))
        .route("/api/tools/{id}/{version}", get(tool))
        .route("/api/composer/check", post(check))
        .route("/api/composer/export", post(export))
        .route("/api/composer/import", post(import))
        .route("/api/composer/run", post(run))
}

/// The tools that match the filters that the query of the address gives.
async fn tools(
    State(state): State<Shared>,
    Query(filter): Query<ToolFilter>,
) -> Result<Json<Value>, Failure> {
    public(&state.studio()?.tools(filter).await?)
}

async fn tool(
    State(state): State<Shared>,
    Path((id, version)): Path<(String, String)>,
) -> Result<Json<Value>, Failure> {
    public(&state.studio()?.tool(id, version).await?)
}

async fn scan(
    State(state): State<Shared>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, Failure> {
    public(&state.studio()?.scan(typed(request)?).await?)
}

async fn suggest(
    State(state): State<Shared>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, Failure> {
    public(&state.studio()?.suggest(typed(request)?).await?)
}

async fn check(
    State(state): State<Shared>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, Failure> {
    public(&state.studio()?.check(typed(request)?).await?)
}

async fn export(
    State(state): State<Shared>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, Failure> {
    public(&state.studio()?.export(typed(request)?).await?)
}

async fn import(
    State(state): State<Shared>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, Failure> {
    public(&state.studio()?.import(typed(request)?).await?)
}

/// Lowers a composed workflow and starts it like any other run.
async fn run(State(state): State<Shared>, Json(request): Json<Value>) -> Result<Response, Failure> {
    let started = state.studio()?.run(typed(request)?).await?;
    accepted(&state, &started)
}
