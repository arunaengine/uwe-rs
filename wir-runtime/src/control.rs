//! Control facts of a run: the truth of a tree of status tests, and the result of a status
//! group once its members ended.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use wir::{Kind, ResultKind, Status};

use crate::plan::ControlPlan;
use crate::state::{Progress, State};

/// The truth of a control tree: a test on an op that has not ended yet is pending.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Truth {
    Pending,
    True,
    False,
}

impl Progress {
    /// The truth of a control tree, pending while any op it tests has no control result.
    pub fn control(&self, tree: &ControlPlan) -> Truth {
        match self.resolved(tree) {
            true => self.truth(tree),
            false => Truth::Pending,
        }
    }

    /// Whether every op that a control tree tests has a control result.
    fn resolved(&self, tree: &ControlPlan) -> bool {
        match tree {
            ControlPlan::All(items) | ControlPlan::Any(items) => {
                items.iter().all(|item| self.resolved(item))
            }
            ControlPlan::Not(inner) => self.resolved(inner),
            ControlPlan::Test(node, _) => {
                self.result(*node).is_some() || self.daemons.contains(node)
            }
        }
    }

    /// The truth of a control tree whose tests all have results.
    fn truth(&self, tree: &ControlPlan) -> Truth {
        match tree {
            ControlPlan::All(items) => {
                let truths: Vec<Truth> = items.iter().map(|item| self.truth(item)).collect();
                match (
                    truths.contains(&Truth::False),
                    truths.contains(&Truth::Pending),
                ) {
                    (true, _) => Truth::False,
                    (false, true) => Truth::Pending,
                    (false, false) => Truth::True,
                }
            }
            ControlPlan::Any(items) => {
                let truths: Vec<Truth> = items.iter().map(|item| self.truth(item)).collect();
                match (
                    truths.contains(&Truth::True),
                    truths.contains(&Truth::Pending),
                ) {
                    (true, _) => Truth::True,
                    (false, true) => Truth::Pending,
                    (false, false) => Truth::False,
                }
            }
            ControlPlan::Not(inner) => match self.truth(inner) {
                Truth::True => Truth::False,
                Truth::False => Truth::True,
                Truth::Pending => Truth::Pending,
            },
            ControlPlan::Test(node, results) => {
                let status = self.result(*node);
                let ready = self.daemons.contains(node);
                let holds = |result: &ResultKind| match status {
                    Some(status) => self.holds(*node, status, *result),
                    // A daemon that runs has only its readiness as its result.
                    None => *result == ResultKind::Daemoned && ready,
                };
                match (status.is_some() || ready, results.iter().any(holds)) {
                    (false, _) => Truth::Pending,
                    (true, true) => Truth::True,
                    (true, false) => Truth::False,
                }
            }
        }
    }

    /// The status a node ended with, which settles its control result.
    fn result(&self, node: usize) -> Option<Status> {
        match &self.states[node] {
            State::Ended(status, _) => Some(*status),
            _ => None,
        }
    }

    /// Whether an ended node has a result; a daemon keeps its readiness once it ends.
    fn holds(&self, node: usize, status: Status, result: ResultKind) -> bool {
        let members = self.members(node);
        let statuses: Vec<Option<Status>> =
            members.iter().map(|member| self.result(*member)).collect();
        match result {
            ResultKind::Succeeded => status == Status::Succeeded,
            ResultKind::Failed => status == Status::Failed,
            ResultKind::Errored => status == Status::Errored,
            ResultKind::Skipped => status == Status::Skipped,
            ResultKind::Omitted => status == Status::Omitted,
            ResultKind::Cancelled => status == Status::Cancelled,
            ResultKind::Daemoned => self.daemons.contains(&node),
            ResultKind::AnySucceeded => statuses.contains(&Some(Status::Succeeded)),
            ResultKind::AllFailed => {
                !statuses.is_empty() && statuses.iter().all(|item| *item == Some(Status::Failed))
            }
        }
    }

    /// The direct members of a group: those of a status group, or the rounds of a map.
    fn members(&self, node: usize) -> Vec<usize> {
        match &self.node(node).op.kind {
            Kind::StatusGroup(_) => self.node(node).members.clone(),
            _ => (0..self.plan.nodes.len())
                .filter(|at| self.node(*at).parent == Some(node))
                .collect(),
        }
    }

    /// The status of a status group once every member ended: cancelled when one was, else
    /// errored or failed when a member ended so and its group does not allow it.
    pub fn group_status(&self, at: usize) -> Option<Status> {
        let Kind::StatusGroup(group) = &self.node(at).op.kind else {
            return None;
        };
        let statuses = (self.node(at).members.iter())
            .map(|member| self.result(*member))
            .collect::<Option<Vec<Status>>>()?;
        let counted = |wanted: Status| statuses.contains(&wanted) && !group.allow.contains(&wanted);
        Some(match () {
            _ if statuses.contains(&Status::Cancelled) => Status::Cancelled,
            _ if counted(Status::Errored) => Status::Errored,
            _ if counted(Status::Failed) => Status::Failed,
            _ => Status::Succeeded,
        })
    }
}
