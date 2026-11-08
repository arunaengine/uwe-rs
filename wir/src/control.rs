//! Control dependencies: boolean trees of status tests, and status groups.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde::{Deserialize, Serialize};

use crate::op::Status;

/// A boolean tree of status tests on ops of the same graph. An empty `all` holds and an
/// empty `any` does not; a test on an op that has not ended is unresolved, also under `not`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Control {
    All(Vec<Control>),
    Any(Vec<Control>),
    Not(Box<Control>),
    Result(StatusTest),
}

impl Control {
    /// The status tests of the tree, in order.
    pub fn tests(&self) -> Vec<&StatusTest> {
        match self {
            Control::All(items) | Control::Any(items) => {
                items.iter().flat_map(Control::tests).collect()
            }
            Control::Not(inner) => inner.tests(),
            Control::Result(test) => vec![test],
        }
    }
}

/// Holds when the op `op` has one of the results in `on`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusTest {
    pub op: String,
    pub on: Vec<ResultKind>,
}

/// A result an op can have: how it ended, that its daemon is ready, or a result of a group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultKind {
    Succeeded,
    Failed,
    Errored,
    Skipped,
    Omitted,
    Cancelled,
    Daemoned,
    /// A group with a member that succeeded.
    AnySucceeded,
    /// A group with members that all failed.
    AllFailed,
}

impl ResultKind {
    /// Whether the result is about the members of a group.
    pub fn of_group(&self) -> bool {
        matches!(self, ResultKind::AnySucceeded | ResultKind::AllFailed)
    }
}

/// Waits for its members, ops of the same graph, and ends errored when a member errored,
/// failed when one failed, and succeeded otherwise; statuses in `allow` do not count.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusGroup {
    pub members: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<Status>,
}
