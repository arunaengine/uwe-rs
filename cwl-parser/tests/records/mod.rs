//! The records of the CWL test workflows that cannot run: those that cwltool and the loader
//! reject, in `cwl-parser/tests/fixtures/test-workflows/rejected.txt`, and those whose graph or
//! example job fails, in `graph_evidence.txt` and `job_evidence.txt` of
//! `cwl-runtime/tests/fixtures`. A record is a block of lines between empty lines: the
//! workflows it is about, one per line, then `= <kind>: <why>` with one plain sentence, then
//! the evidence that the reference tool, cwltool, fails the same way: `$` the command, and `>`
//! the lines of its output. The kind is `invalid` (the workflow itself is broken), `missing`
//! (it needs something that is not public or not in its repository), or
//! `unsupported (decision N)` (iunctus leaves the feature out on purpose, as decision N of
//! docs/decisions.md says).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

/// The reason of each workflow that a record names. Panics on a record without a valid
/// reason or without evidence, so that the record files stay complete.
pub fn reasons(text: &str) -> BTreeMap<&str, &str> {
    let mut reasons = BTreeMap::new();
    for block in text.split("\n\n") {
        let lines: Vec<&str> = block
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect();
        let subjects: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|line| !line.starts_with(['=', '$', '>']))
            .collect();
        if subjects.is_empty() {
            continue;
        }
        let reason = lines.iter().find_map(|line| line.strip_prefix("= "));
        let Some(reason) = reason.filter(|reason| valid(reason)) else {
            panic!(
                "the record of {subjects:?} needs a line `= invalid: <why>`, \
                 `= missing: <why>` or `= unsupported (decision N): <why>`"
            );
        };
        assert!(
            lines.iter().any(|line| line.starts_with('>')),
            "the record of {subjects:?} shows no output of cwltool"
        );
        for subject in subjects {
            reasons.insert(subject, reason);
        }
    }
    reasons
}

/// Whether a reason names one of the three kinds and says why.
fn valid(reason: &str) -> bool {
    let Some((kind, why)) = reason.split_once(": ") else {
        return false;
    };
    let decision = kind
        .strip_prefix("unsupported (decision ")
        .and_then(|rest| rest.strip_suffix(')'))
        .is_some_and(|number| number.parse::<u32>().is_ok());
    (matches!(kind, "invalid" | "missing") || decision) && !why.trim().is_empty()
}
