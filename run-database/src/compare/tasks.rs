//! What differs between a task of one run and the same task of the other: what it does, its
//! inputs and outputs by content, and how it ended.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap};

use super::{Loaded, Pairing, Side, TaskPair};
use crate::FileRow;
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
pub(super) fn compared(
    a: &Loaded,
    b: &Loaded,
    a_inputs: &HashMap<String, String>,
    b_inputs: &HashMap<String, String>,
) -> TaskPair {
    // Images compare as they ran, pinned to their digests, where both runs know them: a tag
    // and its digest are the same image, and a tag that moved is another one.
    let ran_in = |task: &Loaded| {
        let mut description = task.description.clone();
        if task.side.image.is_some() {
            description.image.clone_from(&task.side.image);
        }
        description
    };
    let mut changes = described(&ran_in(a), &ran_in(b));
    changes.extend(input_changes(a, b, a_inputs, b_inputs));
    // The fingerprint holds the image as the workflow named it, which may be a tag in one run
    // and its digest in the other, as in a replay.
    let named_alike = a.description.image == b.description.image;
    if changes.is_empty() && a.fingerprint != b.fingerprint && named_alike {
        changes
            .push("an input file was written again: its size is the same, its time is new".into());
    }
    let outputs = output_changes(&a.outputs, &b.outputs);
    let pairing = match (changes.is_empty(), outputs.is_empty()) {
        (false, _) => Pairing::Changed,
        (true, false) => Pairing::Outputs,
        (true, true) => Pairing::Same,
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

/// The inputs that differ, by the path the task sees: by content where both digests are known,
/// else by where they come from and their size.
fn input_changes(
    a: &Loaded,
    b: &Loaded,
    a_digests: &HashMap<String, String>,
    b_digests: &HashMap<String, String>,
) -> Vec<String> {
    let by_path = |files: &[FileRow]| -> BTreeMap<String, FileRow> {
        (files.iter())
            .map(|file| (file.path.clone(), file.clone()))
            .collect()
    };
    let (first, second) = (by_path(&a.inputs), by_path(&b.inputs));
    let mut changes = Vec::new();
    for (path, x) in &first {
        let Some(y) = second.get(path) else {
            changes.push(format!("only the first run reads the input `{path}`"));
            continue;
        };
        // What the workflow writes itself is part of the description, compared before.
        if x.location.is_empty() && y.location.is_empty() {
            continue;
        }
        match (a_digests.get(path), b_digests.get(path)) {
            (Some(p), Some(q)) if p == q => {}
            (Some(_), Some(_)) => {
                changes.push(format!("the content of the input `{path}` differs"));
            }
            _ if x.size != y.size => changes.push(format!(
                "the input `{path}` has {} in the first run and {} in the second",
                size(x.size),
                size(y.size)
            )),
            _ if x.location != y.location => changes.push(format!(
                "the input `{path}` comes from {} in the first run and from {} in the second",
                x.location, y.location
            )),
            _ => {}
        }
    }
    for path in second.keys().filter(|path| !first.contains_key(*path)) {
        changes.push(format!("only the second run reads the input `{path}`"));
    }
    changes
}

/// The outputs whose content differs by digest, or that only one run has.
fn output_changes(a: &[FileRow], b: &[FileRow]) -> Vec<String> {
    let by_path = |files: &[FileRow]| -> BTreeMap<String, Option<String>> {
        (files.iter())
            .map(|file| (file.path.clone(), file.digest.clone()))
            .collect()
    };
    let (first, second) = (by_path(a), by_path(b));
    let mut changes = Vec::new();
    for (path, x) in &first {
        match second.get(path) {
            Some(y) => {
                if let (Some(x), Some(y)) = (x, y)
                    && x != y
                {
                    changes.push(format!(
                        "the output `{path}` differs: {} in the first run, {} in the second",
                        short(x),
                        short(y)
                    ));
                }
            }
            // An output of a task that did not finish is no difference of content.
            None if !b.is_empty() => changes.push(format!("only the first run made `{path}`")),
            None => {}
        }
    }
    if !a.is_empty() {
        for path in second.keys().filter(|path| !first.contains_key(*path)) {
            changes.push(format!("only the second run made `{path}`"));
        }
    }
    changes
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

/// A digest in short, like `sha256:1f2e3d4c5b6a`.
fn short(digest: &str) -> String {
    match digest.split_once(':') {
        Some((kind, hex)) => format!("{kind}:{}", &hex[..hex.len().min(12)]),
        None => digest.chars().take(19).collect(),
    }
}

fn size(size: Option<u64>) -> String {
    size.map_or_else(
        || "an unknown size".to_owned(),
        |size| format!("{size} bytes"),
    )
}
