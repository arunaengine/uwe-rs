//! Validates rejection records and their reference-tool evidence for CWL workflows.
// Copyright (c) 2026 The uwe Contributors
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
