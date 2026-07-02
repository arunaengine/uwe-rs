//! What Nextflow does when a task fails: its `errorStrategy`, and `maxRetries` for `retry`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::interpreter::TaskDirective;
use nextflow_evaluator::value::Value;

/// An error strategy of Nextflow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Strategy {
    /// Stop the run at once; the default.
    Terminate,
    /// Start no more tasks, let the running ones end, then stop the run.
    Finish,
    /// Go on as if the task had not run: its outputs stay empty.
    Ignore,
    /// Run the task again, with its attempt counted up, up to `maxRetries` times.
    Retry,
}

/// The strategy and the number of retries that the directives of a failed task give;
/// Nextflow's defaults are `terminate` and one retry.
pub(crate) fn of(directives: &[TaskDirective]) -> (Strategy, u32) {
    let value = |name: &str| {
        directives
            .iter()
            .rev()
            .find(|directive| &*directive.name == name)
            .and_then(|directive| directive.arguments.first())
    };
    let strategy = match value("errorStrategy").map(Value::to_string).as_deref() {
        Some(text) if text.eq_ignore_ascii_case("retry") => Strategy::Retry,
        Some(text) if text.eq_ignore_ascii_case("ignore") => Strategy::Ignore,
        Some(text) if text.eq_ignore_ascii_case("finish") => Strategy::Finish,
        _ => Strategy::Terminate,
    };
    let retries = match value("maxRetries") {
        Some(Value::Number(number)) => number.to_i64().clamp(0, u32::MAX as i64) as u32,
        Some(Value::String(text)) => text.trim().parse().unwrap_or(1),
        _ => 1,
    };
    (strategy, retries)
}
