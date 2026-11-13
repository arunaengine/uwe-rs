//! Enforces workflow and node deadlines through individual engine cancellation.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::{Duration, Instant};

use workflow_graph::TaskId;

use crate::node::{NodeKind, Phase};
use crate::run::{Failure, Result, Run};
use crate::tags::Variables;

pub(crate) struct Limit {
    pub at: Instant,
    pub message: String,
}

#[cfg(test)]
#[path = "timers_tests.rs"]
mod tests;

pub(crate) fn seconds(text: &str, zero: bool) -> Result<Duration> {
    text.trim().parse::<u64>().ok().filter(|value| zero || *value > 0)
        .map(Duration::from_secs).ok_or_else(|| Failure::error("activeDeadlineSeconds must be a nonnegative workflow integer or a positive template integer"))
}

impl Run {
    pub fn arm_workflow(&mut self) -> Result<()> {
        if let Some(value) = &self.spec.active_deadline_seconds {
            let text = crate::tags::replace(&value.0, &Variables::new(self.globals.clone()), false)
                .map_err(Failure::error)?;
            self.workflow_limit = Some(
                self.clock
                    .checked_add(seconds(&text, true)?)
                    .ok_or_else(|| Failure::error("workflow deadline overflow"))?,
            );
        }
        Ok(())
    }

    pub fn arm_template(&mut self, index: usize) -> Result<()> {
        let Some(processed) = &self.nodes[index].processed else {
            return Ok(());
        };
        if let Some(value) = &processed.template.active_deadline_seconds {
            seconds(&value.0, false)?;
        }
        if let Some(timeout) = &processed.template.timeout
            && !timeout.0.is_empty()
        {
            let duration = crate::resources::duration(&timeout.0)
                .ok_or_else(|| Failure::error("invalid timeout format"))?;
            let at = self.nodes[index]
                .started_at
                .checked_add(duration)
                .ok_or_else(|| Failure::error("template timeout overflow"))?;
            self.limit_node(index, at, "timeout");
        }
        if let Some(at) = self.nodes[index].retry_limit {
            self.limit_node(index, at, "Max duration limit exceeded");
        }
        Ok(())
    }

    pub fn arm_active(&mut self, index: usize) {
        if !self.nodes[index].tasks.is_empty() {
            return;
        }
        if self.nodes[index].retry_limit.is_some() {
            return;
        }
        let duration = self.nodes[index]
            .processed
            .as_ref()
            .and_then(|processed| processed.template.active_deadline_seconds.as_ref())
            .and_then(|value| seconds(&value.0, false).ok());
        if let Some(duration) = duration
            && let Some(at) = self.clock.checked_add(duration)
        {
            self.limit_node(
                index,
                at,
                "Pod was active on the node longer than the specified deadline",
            );
        }
    }

    pub fn limit_node(&mut self, index: usize, at: Instant, message: &str) {
        if self.deadlines.get(&index).is_none_or(|limit| at < limit.at) {
            self.deadlines.insert(
                index,
                Limit {
                    at,
                    message: message.into(),
                },
            );
        }
    }

    pub fn expire(&mut self) {
        if self.workflow_limit.is_some_and(|at| self.clock >= at) {
            self.workflow_limit = None;
            self.stopped = true;
            for child in &mut self.children {
                child.run.workflow_limit = Some(self.clock);
            }
            for index in 0..self.nodes.len() {
                if !self.nodes[index].phase.fulfilled()
                    && !self.nodes[index].name.contains(".onExit")
                    && (matches!(
                        self.nodes[index].kind,
                        NodeKind::Pod | NodeKind::Plugin | NodeKind::Suspend
                    ) || self.nodes[index].phase == Phase::Pending
                        || self.waits.contains_key(&index))
                {
                    self.expire_node(index, "Step exceeded its deadline".into());
                }
            }
        }
        let expired: Vec<_> = self
            .deadlines
            .iter()
            .filter(|(_, limit)| self.clock >= limit.at)
            .map(|(&index, limit)| (index, limit.message.clone()))
            .collect();
        for (index, message) in expired {
            self.expire_node(index, message);
        }
    }

    fn expire_node(&mut self, index: usize, message: String) {
        if message == "Max duration limit exceeded" {
            self.first_failure = Some(message.clone());
        }
        self.deadlines.remove(&index);
        self.waits.remove(&index);
        let mut descendants = vec![index];
        let mut position = 0;
        while position < descendants.len() {
            descendants.extend(self.nodes[descendants[position]].children.iter().copied());
            position += 1;
        }
        let tasks: Vec<_> = self
            .tasks
            .iter()
            .filter(|(_, (node, _))| descendants.contains(node))
            .map(|(&id, _)| id)
            .collect();
        if tasks.is_empty() {
            self.fail(index, Phase::Failed, message);
        } else {
            for task in &tasks {
                let node = self.tasks[task].0;
                self.deadlines.remove(&node);
                self.expired.insert(node, message.clone());
            }
            self.expired.insert(index, message);
            self.cancel_tasks.extend(tasks);
        }
    }

    pub fn expired_task(&self, task: TaskId) -> bool {
        self.tasks
            .get(&task)
            .is_some_and(|(index, _)| self.expired.contains_key(index))
            || self
                .children
                .iter()
                .any(|child| child.run.expired_task(task))
    }

    pub fn cancellations(&mut self) -> Vec<TaskId> {
        let mut tasks = std::mem::take(&mut self.cancel_tasks);
        for child in &mut self.children {
            tasks.extend(child.run.cancellations());
        }
        tasks
    }
}
