//! What differs between a task of one run and the same task of the other: what it does and
//! how it ended.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::{Loaded, Pairing, Side, TaskPair};
use crate::why::described;

/// A task that only one run has.
pub(super) fn alone(task: &Loaded, pairing: Pairing) -> TaskPair {
    let side = Some(task.side.clone());
    let (first, second) = match pairing {
        Pairing::First => (side, None),
        _ => (None, side),
    };
    TaskPair {
        step: task.step.clone(),
        pairing,
        first,
        second,
        changes: Vec::new(),
        outputs: Vec::new(),
        results: Vec::new(),
    }
}

/// What differs between a task and the same task of the other run.
pub(super) fn compared(a: &Loaded, b: &Loaded) -> TaskPair {
    let mut changes = described(&a.description, &b.description);
    if changes.is_empty() && a.fingerprint != b.fingerprint {
        changes
            .push("an input file was written again: its size is the same, its time is new".into());
    }
    if a.description.image == b.description.image
        && let (Some(x), Some(y)) = (&a.side.image, &b.side.image)
        && x != y
    {
        changes.push(format!(
            "the image ran as `{x}` in the first run and as `{y}` in the second"
        ));
    }
    let outputs = Vec::new();
    let pairing = match changes.is_empty() {
        false => Pairing::Changed,
        true => Pairing::Same,
    };
    TaskPair {
        step: a.step.clone(),
        pairing,
        results: result_changes(&a.side, &b.side),
        first: Some(a.side.clone()),
        second: Some(b.side.clone()),
        changes,
        outputs,
    }
}

/// How the task ended differently in the two runs.
fn result_changes(a: &Side, b: &Side) -> Vec<String> {
    let mut changes = Vec::new();
    let ended = |side: &Side| match (side.state.as_str(), side.exit_code) {
        ("failed", Some(code)) => format!("failed with exit code {code}"),
        (state, _) => state.to_owned(),
    };
    let (x, y) = (ended(a), ended(b));
    if x != y {
        changes.push(format!("it {x} in the first run and {y} in the second"));
    }
    let came = |side: &Side| match side.reused.as_deref() {
        Some("cache") => "came from the cache".to_owned(),
        Some(run) => format!("came from run {run}"),
        None => "ran".to_owned(),
    };
    if a.reused.is_some() != b.reused.is_some() {
        changes.push(format!(
            "it {} in the first run and {} in the second",
            came(a),
            came(b)
        ));
    }
    changes
}
