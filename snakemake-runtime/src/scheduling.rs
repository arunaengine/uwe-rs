//! Holds back jobs while the global resources of the run are taken, as Snakemake's scheduler.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap};

use indexmap::IndexMap;
use python_evaluator::Value;
use workflow_graph::TaskId;

/// What a job takes of each resource while it runs: `_cores` its threads, `_nodes` one, and
/// its integer resources.
pub type Usage = Vec<(String, i64)>;

/// The usage of a job with `threads` and evaluated `resources`.
pub fn usage(threads: i64, resources: &[(String, Value)]) -> Usage {
    let mut usage = vec![("_cores".to_owned(), threads), ("_nodes".to_owned(), 1)];
    for (name, value) in resources {
        let Value::Int(amount) = value.plain() else {
            continue;
        };
        // A job that takes `_cores` or `_nodes` itself, like a handover job, takes the most.
        match usage.iter_mut().find(|(used, _)| used == name) {
            Some((_, taken)) => *taken = (*taken).max(*amount),
            None => usage.push((name.clone(), *amount)),
        }
    }
    usage
}

/// The usage of a task of several jobs: the jobs of one layer run at once and add up, the
/// layers run one after the other, so the task takes what its largest layer does; one node.
pub fn combined(usages: &[Usage], layers: &[usize]) -> Usage {
    let mut sums: BTreeMap<usize, BTreeMap<&str, i64>> = BTreeMap::new();
    for (usage, layer) in usages.iter().zip(layers) {
        for (name, amount) in usage {
            *sums.entry(*layer).or_default().entry(name).or_default() += amount;
        }
    }
    let mut most: BTreeMap<&str, i64> = BTreeMap::new();
    for sum in sums.values() {
        for (name, amount) in sum {
            let entry = most.entry(name).or_default();
            *entry = (*entry).max(*amount);
        }
    }
    most.insert("_nodes", 1);
    most.into_iter()
        .map(|(name, amount)| (name.to_owned(), amount))
        .collect()
}

/// The limits of the global integer resources. As in Snakemake, `_cores` limits the threads of
/// all jobs only when `_nodes` does not limit their number.
pub fn limits(globals: &IndexMap<String, Value>) -> BTreeMap<String, i64> {
    let mut limits: BTreeMap<String, i64> = (globals.iter())
        .filter_map(|(name, value)| match value.plain() {
            Value::Int(limit) => Some((name.clone(), *limit)),
            _ => None,
        })
        .collect();
    if limits.contains_key("_nodes") {
        limits.remove("_cores");
    }
    limits
}

/// The usage of the tasks that run.
#[derive(Debug, Default)]
pub struct Gate {
    taken: HashMap<TaskId, Usage>,
}

impl Gate {
    /// Whether a job fits beside the running ones. A job always fits when none runs, so that a
    /// job larger than a limit cannot wait forever.
    pub fn fits(&self, usage: &Usage, limits: &BTreeMap<String, i64>) -> bool {
        if self.taken.is_empty() {
            return true;
        }
        usage.iter().all(|(name, amount)| match limits.get(name) {
            Some(limit) => self.in_use(name).saturating_add(*amount) <= *limit,
            None => true,
        })
    }

    pub fn take(&mut self, task: TaskId, usage: Usage) {
        self.taken.insert(task, usage);
    }

    /// Frees what a task took, and gives it back.
    pub fn release(&mut self, task: TaskId) -> Option<Usage> {
        self.taken.remove(&task)
    }

    fn in_use(&self, name: &str) -> i64 {
        (self.taken.values().flatten())
            .filter(|(used, _)| used == name)
            .map(|(_, amount)| *amount)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holds_shared_resources() {
        let mut globals = IndexMap::new();
        globals.insert("ncbi".to_owned(), Value::Int(1));
        globals.insert("_cores".to_owned(), Value::Int(8));
        let limits = limits(&globals);
        let job = usage(4, &[("ncbi".to_owned(), Value::Int(1))]);
        let mut gate = Gate::default();
        assert!(gate.fits(&job, &limits));
        gate.take(TaskId(1), job.clone());
        assert!(!gate.fits(&job, &limits), "ncbi is taken");
        let other = usage(4, &[]);
        assert!(gate.fits(&other, &limits), "four of eight cores are free");
        gate.release(TaskId(1));
        assert!(gate.fits(&job, &limits));
        // Jobs of one layer of a group add up, layers take turns, and a group is one node.
        let usages = [usage(2, &[]), usage(3, &[]), usage(4, &[])];
        let group = combined(&usages, &[0, 0, 1]);
        assert!(group.contains(&("_cores".to_owned(), 5)), "{group:?}");
        assert!(group.contains(&("_nodes".to_owned(), 1)), "{group:?}");
        // With `_nodes`, the cores of all jobs are not limited.
        globals.insert("_nodes".to_owned(), Value::Int(2));
        assert!(!super::limits(&globals).contains_key("_cores"));
    }
}
