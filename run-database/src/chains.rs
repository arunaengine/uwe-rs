//! How long the work is that follows from each step of a workflow, from the times of its earlier
//! runs, so that a run starts the tasks on the longest remaining chain first and ends sooner.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeSet, HashMap, HashSet};

use rusqlite::params;

use crate::{Database, DatabaseError};

/// How many of the latest real runs of a workflow the times come from.
const RUNS: u32 = 5;

impl Database {
    /// For each step of a workflow, the time in milliseconds of the longest chain of steps that
    /// starts with it: its own time, and the time of the longest chain after it. The times come
    /// from the latest real runs of the workflow. The time of a step is the mean time of its
    /// tasks that ran; a task taken from the cache or from an earlier run took no time.
    pub fn chains(&self, workflow: &str) -> Result<HashMap<String, u64>, DatabaseError> {
        let mut statement = self.connection.prepare(
            "SELECT id FROM runs WHERE workflow = ?1 AND simulated = 0
             ORDER BY started DESC LIMIT ?2",
        )?;
        let runs = statement
            .query_map(params![workflow, RUNS], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut times: HashMap<String, (u64, u64)> = HashMap::new();
        let mut after: HashMap<String, BTreeSet<String>> = HashMap::new();
        let mut timed = self.connection.prepare(
            "SELECT t.step, a.ended - a.started FROM attempts a
             JOIN tasks t ON t.run = a.run AND t.id = a.task
             WHERE a.run = ?1 AND a.reused IS NULL AND a.exit_code = 0 AND a.ended IS NOT NULL",
        )?;
        let mut linked = self.connection.prepare(
            "SELECT DISTINCT s.step, t.step FROM edges e
             JOIN tasks s ON s.run = e.run AND s.id = e.source
             JOIN tasks t ON t.run = e.run AND t.id = e.target
             WHERE e.run = ?1 AND s.step <> t.step",
        )?;
        for run in &runs {
            let rows = timed.query_map([run], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (step, time) = row?;
                let (sum, count) = times.entry(step).or_default();
                *sum += time.max(0) as u64;
                *count += 1;
            }
            let rows = linked.query_map([run], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (from, to) = row?;
                after.entry(from).or_default().insert(to);
            }
        }
        let mean: HashMap<String, u64> = (times.into_iter())
            .map(|(step, (sum, count))| (step, sum / count.max(1)))
            .collect();
        let steps: BTreeSet<&String> = mean.keys().chain(after.keys()).collect();
        let mut chains = HashMap::new();
        for step in steps {
            longest(step, &mean, &after, &mut chains, &mut HashSet::new());
        }
        Ok(chains)
    }
}

/// The time of the longest chain that starts with `step`, remembered in `chains`. A step that
/// the chain reached before stops it, so that a loop between steps ends.
fn longest(
    step: &str,
    times: &HashMap<String, u64>,
    after: &HashMap<String, BTreeSet<String>>,
    chains: &mut HashMap<String, u64>,
    on_the_way: &mut HashSet<String>,
) -> u64 {
    if let Some(&known) = chains.get(step) {
        return known;
    }
    if !on_the_way.insert(step.to_owned()) {
        return 0;
    }
    let next = (after.get(step).into_iter().flatten())
        .map(|next| longest(next, times, after, chains, on_the_way))
        .max()
        .unwrap_or_default();
    on_the_way.remove(step);
    let total = times.get(step).copied().unwrap_or_default() + next;
    chains.insert(step.to_owned(), total);
    total
}
