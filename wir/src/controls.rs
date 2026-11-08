//! Verifier checks of control dependencies: the ops that trees, handlers and groups name,
//! the results they test, and the statuses that handlers take.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;

use super::{Checker, Problem};
use crate::control::ResultKind;
use crate::document::Graph;
use crate::op::{Kind, Op, Status};

impl<'d> Checker<'d> {
    pub(super) fn control(&mut self, at: &str, graph: &Graph, op: &Op) {
        let find = |name: &str| graph.ops.iter().find(|item| item.path == name);
        for test in op.depends.iter().flat_map(|tree| tree.tests()) {
            let Some(target) = find(&test.op) else {
                self.report(at, Problem::NoOp(test.op.clone()));
                continue;
            };
            if test.on.is_empty() {
                self.report(at, Problem::Empty(format!("the results of {}", test.op)));
            }
            for result in &test.on {
                let fits = match result {
                    _ if result.of_group() => grouped(&target.kind),
                    ResultKind::Daemoned => daemons(target),
                    _ => true,
                };
                if !fits {
                    let why = format!("{} to have the result {result:?}", test.op);
                    self.report(at, Problem::Shape("a control test".into(), why));
                }
            }
        }
        for handled in &op.handles {
            if find(&handled.op).is_none() {
                self.report(at, Problem::NoOp(handled.op.clone()));
            }
            if handled.op == op.path {
                let why = "another op to handle".into();
                self.report(at, Problem::Shape("a handler".into(), why));
            }
            if handled.on.is_empty() {
                self.report(
                    at,
                    Problem::Empty(format!("the statuses of {}", handled.op)),
                );
            }
            let other = handled
                .on
                .iter()
                .find(|status| !matches!(status, Status::Failed | Status::Errored));
            if let Some(status) = other {
                let why = format!("failed or errored, not {status:?}");
                self.report(at, Problem::Shape("a handler".into(), why));
            }
        }
        if let Kind::StatusGroup(group) = &op.kind {
            let mut seen = BTreeSet::new();
            for member in &group.members {
                if find(member).is_none() {
                    self.report(at, Problem::NoOp(member.clone()));
                }
                if *member == op.path {
                    let why = "members besides itself".into();
                    self.report(at, Problem::Shape("a status group".into(), why));
                }
                if !seen.insert(member) {
                    self.report(at, Problem::Duplicate(format!("member {member}")));
                }
            }
        }
    }
}

/// Whether an op has the results of a group: its members or its rounds.
fn grouped(kind: &Kind) -> bool {
    matches!(kind, Kind::StatusGroup(_) | Kind::Map(_) | Kind::Process(_))
}

/// Whether an op can report that its daemon is ready.
fn daemons(_op: &Op) -> bool {
    false
}

/// The ops an op's control waits for: the ops its tree tests, its group members and the ops
/// whose failures it handles.
pub(super) fn waits_for(op: &Op) -> Vec<&str> {
    let tests = op
        .depends
        .iter()
        .flat_map(|tree| tree.tests())
        .map(|test| test.op.as_str());
    let handled = op.handles.iter().map(|item| item.op.as_str());
    let members: Vec<&str> = match &op.kind {
        Kind::StatusGroup(group) => group.members.iter().map(String::as_str).collect(),
        _ => Vec::new(),
    };
    tests.chain(handled).chain(members).collect()
}
