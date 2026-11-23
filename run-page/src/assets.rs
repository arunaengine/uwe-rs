//! The page itself: one HTML file, its style and its scripts, inside the binary.
// Copyright (c) 2026 The uwe Contributors
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
const STUDIO_STYLE: &str = include_str!("../assets/studio.css");
const FORM_SCRIPT: &str = include_str!("../assets/schema_form.js");
const LAUNCH_SCRIPT: &str = include_str!("../assets/launch.js");
const CANVAS_SCRIPT: &str = include_str!("../assets/canvas.js");
const COMPOSED_SCRIPT: &str = include_str!("../assets/composed.js");
const EXPLORER_SCRIPT: &str = include_str!("../assets/explorer.js");
const INSPECTOR_SCRIPT: &str = include_str!("../assets/inspector.js");
const COMPOSER_SCRIPT: &str = include_str!("../assets/composer.js");
const ICON: &str = include_str!("../assets/icon.svg");

/// The same page for every view; its script shows the view that the address names.
pub(crate) async fn page() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], PAGE)
}

pub(crate) async fn style() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], STYLE)
}

/// The style of the launcher and the combiner, which a report does not need.
pub(crate) async fn studio_style() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STUDIO_STYLE,
    )
}

/// The mark of uwe, the icon of the browser tab.
pub(crate) async fn icon() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/svg+xml")], ICON)
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

/// The form of a JSON Schema, which the launcher and the combiner share.
pub(crate) async fn form_script() -> impl IntoResponse {
    javascript(FORM_SCRIPT)
}

/// Starting a workflow from the page.
pub(crate) async fn launch_script() -> impl IntoResponse {
    javascript(LAUNCH_SCRIPT)
}

/// The canvas of the combiner: nodes, ports, edges, moving and zooming.
pub(crate) async fn canvas_script() -> impl IntoResponse {
    javascript(CANVAS_SCRIPT)
}

/// The model of the combiner and the JSON form of its composed workflow.
pub(crate) async fn composed_script() -> impl IntoResponse {
    javascript(COMPOSED_SCRIPT)
}

/// The task explorer of the combiner.
pub(crate) async fn explorer_script() -> impl IntoResponse {
    javascript(EXPLORER_SCRIPT)
}

/// The inspector of the combiner: the selection, the parameters and the check.
pub(crate) async fn inspector_script() -> impl IntoResponse {
    javascript(INSPECTOR_SCRIPT)
}

/// The state, toolbar, wiring and dialogs of the combiner.
pub(crate) async fn composer_script() -> impl IntoResponse {
    javascript(COMPOSER_SCRIPT)
}

fn javascript(text: &'static str) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        text,
    )
}
