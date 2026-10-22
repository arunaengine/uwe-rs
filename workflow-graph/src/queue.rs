//! The tasks of a run that have not started yet, and the order rules between them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::{Task, TaskId};

/// Tasks in the order the driver asked for them. A task can start when every task in its
/// `after` list has finished.
#[derive(Default)]
pub(crate) struct Queue {
    waiting: BTreeMap<u64, Waiting>,
    ready: BTreeSet<u64>,
    dependents: HashMap<TaskId, HashSet<u64>>,
    finished: HashSet<TaskId>,
    sequence: u64,
}

struct Waiting {
    task: Task,
    missing: usize,
}

impl Queue {
    pub(crate) fn push(&mut self, task: Task) {
        if self.finished.remove(&task.id)
            && let Some(dependents) = self.dependents.get(&task.id)
        {
            for index in dependents {
                self.waiting.get_mut(index).unwrap().missing += 1;
                self.ready.remove(index);
            }
        }
        let index = self.sequence;
        self.sequence += 1;
        let mut missing = 0;
        for id in &task.after {
            if self.dependents.entry(*id).or_default().insert(index) && !self.finished.contains(id)
            {
                missing += 1;
            }
        }
        if missing == 0 {
            self.ready.insert(index);
        }
        self.waiting.insert(index, Waiting { task, missing });
    }

    /// Takes the first task that can start, if there is one.
    pub(crate) fn next(&mut self) -> Option<Task> {
        let index = self.ready.pop_first()?;
        let waiting = self.waiting.remove(&index).unwrap();
        for id in &waiting.task.after {
            if let Some(dependents) = self.dependents.get_mut(id) {
                dependents.remove(&index);
                if dependents.is_empty() {
                    self.dependents.remove(id);
                }
            }
        }
        Some(waiting.task)
    }

    /// Drops the tasks that have not started, which never will.
    pub(crate) fn clear(&mut self) {
        self.waiting.clear();
        self.ready.clear();
        self.dependents.clear();
    }

    /// Notes that a task finished, so that the tasks after it can start.
    pub(crate) fn finished(&mut self, id: TaskId) {
        if self.finished.insert(id)
            && let Some(dependents) = self.dependents.get(&id)
        {
            for index in dependents {
                let waiting = self.waiting.get_mut(index).unwrap();
                waiting.missing -= 1;
                if waiting.missing == 0 {
                    self.ready.insert(*index);
                }
            }
        }
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

    #[test]
    fn retry_closes_gate() {
        let mut queue = Queue::default();
        queue.finished(TaskId(1));
        queue.push(task(1, &[]));
        queue.push(task(2, &[1]));

        assert_eq!(queue.next().map(|task| task.id), Some(TaskId(1)));
        assert!(queue.next().is_none());
        queue.finished(TaskId(1));
        assert_eq!(queue.next().map(|task| task.id), Some(TaskId(2)));
    }

    #[test]
    fn recloses_ready_dependents() {
        let mut queue = Queue::default();
        queue.finished(TaskId(1));
        queue.push(task(2, &[1, 1]));
        queue.push(task(1, &[]));
        queue.push(task(3, &[]));
        assert_eq!(queue.next().map(|task| task.id), Some(TaskId(1)));
        queue.finished(TaskId(1));
        queue.finished(TaskId(1));
        assert_eq!(queue.next().map(|task| task.id), Some(TaskId(2)));
        assert_eq!(queue.next().map(|task| task.id), Some(TaskId(3)));
        assert!(queue.dependents.is_empty());
    }

    #[test]
    fn unlocks_requested_order() {
        let mut queue = Queue::default();
        queue.push(task(1, &[]));
        for id in 2..=10_000 {
            queue.push(task(id, &[1]));
        }
        queue.push(task(10_001, &[]));
        assert_eq!(queue.next().map(|task| task.id), Some(TaskId(1)));
        assert_eq!(queue.next().map(|task| task.id), Some(TaskId(10_001)));
        queue.finished(TaskId(1));
        for id in 2..=10_000 {
            assert_eq!(queue.next().map(|task| task.id), Some(TaskId(id)));
        }
        assert!(queue.next().is_none());
        assert!(queue.dependents.is_empty());
    }
}
