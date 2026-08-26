//! The page itself: one HTML file, its style and its scripts, inside the binary.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use axum::http::header;
use axum::response::IntoResponse;

const PAGE: &str = include_str!("../assets/index.html");
pub(crate) const STYLE: &str = include_str!("../assets/page.css");
pub(crate) const SCRIPT: &str = include_str!("../assets/page.js");
pub(crate) const RUN_SCRIPT: &str = include_str!("../assets/run.js");
pub(crate) const GRAPH_SCRIPT: &str = include_str!("../assets/graph.js");
pub(crate) const CAMERA_SCRIPT: &str = include_str!("../assets/camera.js");
pub(crate) const BOXES_SCRIPT: &str = include_str!("../assets/boxes.js");
pub(crate) const TIME_SCRIPT: &str = include_str!("../assets/time.js");
pub(crate) const COMPARE_SCRIPT: &str = include_str!("../assets/compare.js");

/// The same page for every view; its script shows the view that the address names.
pub(crate) async fn page() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], PAGE)
}

pub(crate) async fn style() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], STYLE)
}

/// The helpers, the navigation and the list of runs.
pub(crate) async fn script() -> impl IntoResponse {
    javascript(SCRIPT)
}

/// The view of one run.
pub(crate) async fn run_script() -> impl IntoResponse {
    javascript(RUN_SCRIPT)
}

/// The graph view of a run.
pub(crate) async fn graph_script() -> impl IntoResponse {
    javascript(GRAPH_SCRIPT)
}

/// Moving and zooming the graph view.
pub(crate) async fn camera_script() -> impl IntoResponse {
    javascript(CAMERA_SCRIPT)
}

/// The boxes of the graph view: tooltips, live states and the legend.
pub(crate) async fn boxes_script() -> impl IntoResponse {
    javascript(BOXES_SCRIPT)
}

/// Where the time of a run went, and the critical path in the graph view.
pub(crate) async fn time_script() -> impl IntoResponse {
    javascript(TIME_SCRIPT)
}

/// Comparing two runs.
pub(crate) async fn compare_script() -> impl IntoResponse {
    javascript(COMPARE_SCRIPT)
}

fn javascript(text: &'static str) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        text,
    )
}
