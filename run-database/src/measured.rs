//! What the tasks of each step of a workflow used in its earlier real runs, so that the next run
//! asks for right-sized resources.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use rusqlite::params;
use task_model::Measured;

use crate::{Database, DatabaseError};

/// How many of the latest real runs of a workflow the measures come from.
const RUNS: u32 = 5;

impl Database {
    /// For each step of a workflow, what its tasks used in the latest real runs of the workflow:
    /// the tasks that ran and succeeded, whose executor measured them. Each comes with the
    /// bytes of its input files, where the run knew them.
    pub fn measured(
        &self,
        workflow: &str,
    ) -> Result<HashMap<String, Vec<Measured>>, DatabaseError> {
        let mut statement = self.connection.prepare(
            "SELECT id FROM runs WHERE workflow = ?1 AND simulated = 0
             ORDER BY started DESC LIMIT ?2",
        )?;
        let runs = statement
            .query_map(params![workflow, RUNS], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut used = self.connection.prepare(
            "SELECT t.step, a.ended - a.started, a.cpu_usec, a.memory_peak,
                (SELECT SUM(f.size) FROM files f
                 WHERE f.run = a.run AND f.task = a.task AND f.role = 'input'),
                a.task, a.number
             FROM attempts a
             JOIN tasks t ON t.run = a.run AND t.id = a.task
             WHERE a.run = ?1 AND a.reused IS NULL AND a.exit_code = 0 AND a.ended IS NOT NULL
               AND (a.cpu_usec IS NOT NULL OR a.memory_peak IS NOT NULL)",
        )?;
        let mut measured: HashMap<String, Vec<Measured>> = HashMap::new();
        for run in &runs {
            let handed = self.handed_over(run)?;
            let rows = used.query_map([run], |row| {
                let step: String = row.get(0)?;
                let bytes = |index| {
                    Ok::<_, rusqlite::Error>(
                        row.get::<_, Option<i64>>(index)?
                            .map(|value| value.max(0) as u64),
                    )
                };
                let milliseconds = row.get::<_, i64>(1)?.max(0) as u64;
                let attempt = (row.get::<_, i64>(5)?, row.get::<_, i64>(6)?);
                let task = Measured {
                    milliseconds,
                    cpu_usec: bytes(2)?,
                    memory_peak: bytes(3)?,
                    input_bytes: bytes(4)?,
                    overhead: (handed.get(&attempt))
                        .map(|handed| handed.saturating_sub(milliseconds)),
                };
                Ok((step, task))
            })?;
            for row in rows {
                let (step, task) = row?;
                measured.entry(step).or_default().push(task);
            }
        }
        Ok(measured)
    }

    /// For each attempt of a run, by task and number, the milliseconds from the hand-over of
    /// the task to the executor to its result, from the events of the run.
    fn handed_over(&self, run: &str) -> Result<HashMap<(i64, i64), u64>, DatabaseError> {
        let mut statement = self.connection.prepare(
            "SELECT task, attempt, kind, time FROM events
             WHERE run = ?1 AND kind IN ('started', 'finished') AND task IS NOT NULL
               AND attempt IS NOT NULL",
        )?;
        let rows = statement.query_map([run], |row| {
            Ok((
                (row.get::<_, i64>(0)?, row.get::<_, i64>(1)?),
                row.get::<_, String>(2)? == "started",
                row.get::<_, i64>(3)?,
            ))
        })?;
        let mut times: HashMap<(i64, i64), (Option<i64>, Option<i64>)> = HashMap::new();
        for row in rows {
            let (attempt, started, time) = row?;
            let (start, end) = times.entry(attempt).or_default();
            match started {
                true => *start = Some(time),
                false => *end = Some(time),
            }
        }
        Ok((times.into_iter())
            .filter_map(|(attempt, (start, end))| Some((attempt, (end? - start?).max(0) as u64)))
            .collect())
    }
}
