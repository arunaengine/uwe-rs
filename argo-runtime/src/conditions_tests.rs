//! Tests for the conditions of resource templates, with conditions from the Argo test workflows.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use super::{Verdict, judge, parse};
use crate::value::Value;

fn workflow(phase: &str) -> Value {
    let status = BTreeMap::from([
        ("phase".to_owned(), Value::string(phase)),
        ("succeeded".to_owned(), Value::Float(2.0)),
    ]);
    Value::Map(BTreeMap::from([("status".to_owned(), Value::Map(status))]))
}

const SUCCESS: &str = "status.phase == Succeeded";
const FAILURE: &str = "status.phase in (Failed, Error)";

#[test]
fn formats_kubernetes_requirements() {
    let printed: Vec<String> = parse("status.phase in (Failed, Error), !status.x, a.b != c")
        .expect("parses")
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        printed,
        ["a.b!=c", "status.phase in (Error,Failed)", "!status.x"]
    );
    assert!(parse("status.phase is Failed").is_err());
    assert!(parse("status.succeeded > x").is_err());
}

#[test]
fn judges_workflow_phase() {
    let judged = |phase: &str| judge(SUCCESS, FAILURE, &workflow(phase)).expect("judges");
    assert_eq!(judged("Succeeded"), Verdict::Succeeded);
    assert_eq!(judged("Running"), Verdict::Waiting);
    assert_eq!(
        judged("Error"),
        Verdict::Failed("failure condition 'status.phase in (Error,Failed)' evaluated true".into())
    );
    // Without a success condition, a resource that does not fail succeeds, as in Argo.
    assert_eq!(
        judge("", FAILURE, &workflow("Running")),
        Ok(Verdict::Succeeded)
    );
    let counted = judge(
        "status.succeeded > 1, status.phase",
        "",
        &workflow("Running"),
    );
    assert_eq!(counted, Ok(Verdict::Succeeded));
    let missing = judge(
        "status.missing == x",
        "status.other != y",
        &workflow("Running"),
    );
    assert!(matches!(missing, Ok(Verdict::Failed(_))));
}
