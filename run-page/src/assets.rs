//! The page itself: one HTML file, its style and its script, inside the binary.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use axum::http::header;
use axum::response::IntoResponse;

const PAGE: &str = include_str!("../assets/index.html");
const STYLE: &str = include_str!("../assets/page.css");
const SCRIPT: &str = include_str!("../assets/page.js");
const RUN_SCRIPT: &str = include_str!("../assets/run.js");

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

fn javascript(text: &'static str) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        text,
    )
}
