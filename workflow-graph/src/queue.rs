//! The tasks of a run that have not started yet, and the order rules between them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashSet, VecDeque};

use crate::{Task, TaskId};

/// Tasks in the order the driver asked for them. A task can start when every task in its
/// `after` list has finished.
#[derive(Default)]
pub(crate) struct Queue {
    waiting: VecDeque<Task>,
    finished: HashSet<TaskId>,
}

impl Queue {
    pub(crate) fn push(&mut self, task: Task) {
        self.waiting.push_back(task);
    }

    /// Takes the first task that can start, if there is one.
    pub(crate) fn next(&mut self) -> Option<Task> {
        let ready = |task: &Task| task.after.iter().all(|id| self.finished.contains(id));
        let position = self.waiting.iter().position(ready)?;
        self.waiting.remove(position)
    }

    /// Drops the tasks that have not started, which never will.
    pub(crate) fn clear(&mut self) {
        self.waiting.clear();
    }

    /// Notes that a task finished, so that the tasks after it can start.
    pub(crate) fn finished(&mut self, id: TaskId) {
        self.finished.insert(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: u64, after: &[u64]) -> Task {
        Task {
            id: TaskId(id),
            after: after.iter().copied().map(TaskId).collect(),
            ..Task::default()
        }
    }

    #[test]
    fn waits_preceding_tasks() {
        let mut queue = Queue::default();
        queue.push(task(1, &[]));
        queue.push(task(2, &[1]));
        queue.push(task(3, &[]));
        let first = queue.next().map(|task| task.id);
        assert_eq!(first, Some(TaskId(1)));
        // Task 1 has started but not finished, so task 3 goes first.
        assert_eq!(queue.next().map(|task| task.id), Some(TaskId(3)));
        assert!(queue.next().is_none());
        queue.finished(TaskId(1));
        assert_eq!(queue.next().map(|task| task.id), Some(TaskId(2)));
    }
}
