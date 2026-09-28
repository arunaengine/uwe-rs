//! Where the time of a run went: the critical path, the chain of tasks that set the length of
//! the run; how long tasks waited and how long they ran; and the steps that used the most CPU
//! time, memory and I/O.
//!
//! A task on the critical path waited for the task before it: of the tasks whose files it
//! reads or that it comes after, the one that ended last; or, when the workflow asked for it
//! only after all of those had ended, the task whose end made the workflow ask for it, which
//! the event log names.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::{Database, DatabaseError, now};

/// Where the time of a run went. Times are in milliseconds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TimeSpent {
    pub run: String,
    /// When the run started and ended; for a run that goes on, now.
    pub started: i64,
    pub ended: i64,
    /// The tasks that ran or were asked for, and those whose result came from elsewhere.
    pub tasks: u64,
    pub reused: u64,
    /// The time of all attempts that ran, together.
    pub ran: i64,
    /// The time tasks waited, together: from the moment each could start until it started.
    pub waited: i64,
    /// How long at least one task ran. For the rest of the run, no task ran.
    pub busy: i64,
    /// From the start of the run until the workflow asked for the first task of the path:
    /// iunctus loaded the workflow and checked its inputs, and the workflow planned.
    pub before: i64,
    /// The chain of tasks that set the length of the run, from the first to the last.
    pub critical_path: Vec<PathTask>,
    /// From the end of the last task of the path until the end of the run: iunctus delivered
    /// the results and wrote the reports.
    pub after: i64,
    /// Every step, in the order of its first task.
    pub steps: Vec<StepTime>,
}

/// A task on the critical path, with how its part of the path was spent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PathTask {
    pub task: u64,
    pub name: String,
    pub step: String,
    /// From the end of the task before it until the workflow asked for this one: iunctus
    /// stored the outputs of that task, and the workflow planned the next tasks.
    pub gap: i64,
    /// From the moment it could start until it started.
    pub waited: i64,
    /// From the start of its first attempt until the end of its last.
    pub ran: i64,
    pub attempts: u32,
    /// Where its result came from, when it did not run: `cache` or an earlier run.
    pub reused: Option<String>,
}

/// The tasks of a step, how long they ran and waited, and what they used when the executor
/// measured it: the CPU time and block I/O of all attempts together, and the highest peak of
/// memory of one attempt.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct StepTime {
    pub step: String,
    pub tasks: u64,
    pub ran: i64,
    pub waited: i64,
    pub cpu_usec: Option<i64>,
    pub memory_peak: Option<i64>,
    pub read_bytes: Option<i64>,
    pub write_bytes: Option<i64>,
}

/// A task as the computation needs it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Timed {
    id: u64,
    name: String,
    step: String,
    requested: i64,
    attempts: Vec<Span>,
    /// The tasks whose files it reads or that it comes after.
    before: Vec<u64>,
    /// The task whose end made the workflow ask for it; none for a task of the start.
    trigger: Option<u64>,
}

/// One attempt: when it started and ended, where its result came from when it did not run,
/// and what it used.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Span {
    started: i64,
    ended: Option<i64>,
    reused: Option<String>,
    cpu_usec: Option<i64>,
    memory_peak: Option<i64>,
    read_bytes: Option<i64>,
    write_bytes: Option<i64>,
}

impl Database {
    /// Where the time of a run went; `None` for an unknown run.
    pub fn time_spent(&self, run: &str) -> Result<Option<TimeSpent>, DatabaseError> {
        let Some(record) = self.run(run)? else {
            return Ok(None);
        };
        let tasks = self.timed(run)?;
        let latest = (tasks.iter())
            .flat_map(|task| &task.attempts)
            .flat_map(|span| [Some(span.started), span.ended])
            .flatten()
            .chain(tasks.iter().map(|task| task.requested))
            .max();
        let ended = match record.ended {
            Some(ended) => ended,
            None if record.state == "running" => now(),
            None => latest.unwrap_or(record.started),
        };
        let mut spent = spent(&tasks, record.started, ended.max(record.started));
        spent.run = record.id;
        Ok(Some(spent))
    }

    /// The tasks of a run with their attempts, the tasks before them, and the task whose end
    /// made the workflow ask for each, in the order they were asked for.
    fn timed(&self, run: &str) -> Result<Vec<Timed>, DatabaseError> {
        let mut statement = self
            .connection
            .prepare("SELECT id, name, step, requested FROM tasks WHERE run = ?1 ORDER BY rowid")?;
        let mut tasks = statement
            .query_map([run], |row| {
                Ok(Timed {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    step: row.get(2)?,
                    requested: row.get(3)?,
                    ..Timed::default()
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let index: HashMap<u64, usize> = (tasks.iter().enumerate())
            .map(|(position, task)| (task.id, position))
            .collect();
        let mut statement = self.connection.prepare(
            "SELECT task, started, ended, reused, cpu_usec, memory_peak, read_bytes, write_bytes
             FROM attempts WHERE run = ?1 ORDER BY task, number",
        )?;
        let spans = statement.query_map([run], |row| {
            let span = Span {
                started: row.get(1)?,
                ended: row.get(2)?,
                reused: row.get(3)?,
                cpu_usec: row.get(4)?,
                memory_peak: row.get(5)?,
                read_bytes: row.get(6)?,
                write_bytes: row.get(7)?,
            };
            Ok((row.get::<_, u64>(0)?, span))
        })?;
        for found in spans {
            let (task, span) = found?;
            if let Some(&position) = index.get(&task) {
                tasks[position].attempts.push(span);
            }
        }
        for edge in self.edges(run)? {
            if let Some(&position) = index.get(&edge.target) {
                tasks[position].before.push(edge.source);
            }
        }
        // The engine hears how an attempt ended, then asks the driver, and records the tasks
        // that the driver asks for then: the last end before a request made the request.
        let mut statement = self.connection.prepare(
            "SELECT kind, task FROM events
             WHERE run = ?1 AND kind IN ('requested', 'finished') ORDER BY sequence",
        )?;
        let events = statement.query_map([run], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<u64>>(1)?))
        })?;
        let mut last_end = None;
        let mut asked = HashSet::new();
        for event in events {
            let (kind, task) = event?;
            let Some(task) = task else { continue };
            if kind == "finished" {
                last_end = Some(task);
            } else if asked.insert(task)
                && let Some(&position) = index.get(&task)
            {
                tasks[position].trigger = last_end;
            }
        }
        Ok(tasks)
    }
}

/// Where the time went, from the tasks of a run that started and ended at these times.
fn spent(tasks: &[Timed], started: i64, ended: i64) -> TimeSpent {
    // The end of each task that started: its last attempt ends, or the run does.
    let ends: HashMap<u64, i64> = (tasks.iter())
        .filter_map(|task| Some((task.id, task.attempts.last()?.ended.unwrap_or(ended))))
        .collect();
    let mut spent = TimeSpent {
        started,
        ended,
        tasks: tasks.len() as u64,
        ..TimeSpent::default()
    };
    let mut steps: Vec<StepTime> = Vec::new();
    let mut step_index: HashMap<&str, usize> = HashMap::new();
    let mut intervals = Vec::new();
    for task in tasks {
        let position = *step_index.entry(&task.step).or_insert_with(|| {
            steps.push(StepTime {
                step: task.step.clone(),
                ..StepTime::default()
            });
            steps.len() - 1
        });
        let step = &mut steps[position];
        step.tasks += 1;
        if task
            .attempts
            .last()
            .is_some_and(|last| last.reused.is_some())
        {
            spent.reused += 1;
        }
        let ran: Vec<&Span> = (task.attempts.iter())
            .filter(|span| span.reused.is_none())
            .collect();
        for span in &ran {
            let end = span.ended.unwrap_or(ended);
            let time = (end - span.started).max(0);
            spent.ran += time;
            step.ran += time;
            intervals.push((span.started, end));
            add(&mut step.cpu_usec, span.cpu_usec);
            add(&mut step.read_bytes, span.read_bytes);
            add(&mut step.write_bytes, span.write_bytes);
            step.memory_peak = step.memory_peak.max(span.memory_peak);
        }
        if let Some(first) = ran.first() {
            let waited = (first.started - ready(task, &ends)).max(0);
            spent.waited += waited;
            step.waited += waited;
        }
    }
    spent.busy = union(intervals);
    spent.steps = steps;
    critical_path(&mut spent, tasks, &ends);
    spent
}

/// Adds a measured amount to a sum that is `None` until something was measured.
fn add(sum: &mut Option<i64>, amount: Option<i64>) {
    if let Some(amount) = amount {
        *sum = Some(sum.unwrap_or(0).saturating_add(amount));
    }
}

/// When a task could start: when the workflow had asked for it and every task before it had
/// ended.
fn ready(task: &Timed, ends: &HashMap<u64, i64>) -> i64 {
    (task.before.iter())
        .filter_map(|id| ends.get(id).copied())
        .fold(task.requested, i64::max)
}

/// How long at least one of the intervals lasted.
fn union(mut intervals: Vec<(i64, i64)>) -> i64 {
    intervals.sort_unstable();
    let mut total = 0;
    let mut current: Option<(i64, i64)> = None;
    for (start, end) in intervals {
        match &mut current {
            Some((_, until)) if start <= *until => *until = (*until).max(end),
            _ => {
                if let Some((from, until)) = current {
                    total += until - from;
                }
                current = Some((start, end));
            }
        }
    }
    total + current.map_or(0, |(from, until)| until - from)
}

/// Walks back from the task that ended last, each time to the task it waited for, and splits
/// the length of the run into the parts of that chain.
fn critical_path(spent: &mut TimeSpent, tasks: &[Timed], ends: &HashMap<u64, i64>) {
    let by_id: HashMap<u64, &Timed> = tasks.iter().map(|task| (task.id, task)).collect();
    let last = (tasks.iter())
        .filter_map(|task| Some((*ends.get(&task.id)?, task.id)))
        .max();
    let Some((_, mut current)) = last else {
        spent.before = spent.ended - spent.started;
        return;
    };
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    while seen.insert(current) {
        let Some(task) = by_id.get(&current) else {
            break;
        };
        let latest = (task.before.iter())
            .filter_map(|id| Some((*ends.get(id)?, *id)))
            .max();
        let waited_for = match latest {
            Some((end, id)) if end > task.requested => Some(id),
            _ => (task.trigger)
                .filter(|id| ends.contains_key(id))
                .or(latest.map(|(_, id)| id)),
        };
        chain.push((*task, waited_for));
        match waited_for {
            Some(id) => current = id,
            None => break,
        }
    }
    chain.reverse();
    let mut path = Vec::with_capacity(chain.len());
    for (task, waited_for) in &chain {
        let before_end = waited_for.and_then(|id| ends.get(&id).copied());
        let ready = before_end.map_or(task.requested, |end| end.max(task.requested));
        let first = task.attempts.first().map_or(ready, |span| span.started);
        let end = ends.get(&task.id).copied().unwrap_or(first);
        path.push(PathTask {
            task: task.id,
            name: task.name.clone(),
            step: task.step.clone(),
            gap: before_end.map_or(0, |end| (task.requested - end).max(0)),
            waited: (first - ready).max(0),
            ran: (end - first).max(0),
            attempts: task.attempts.len() as u32,
            reused: task.attempts.last().and_then(|span| span.reused.clone()),
        });
    }
    if let Some((first, _)) = chain.first() {
        spent.before = (first.requested - spent.started).max(0);
    }
    if let Some((last, _)) = chain.last() {
        let end = ends.get(&last.id).copied().unwrap_or(spent.ended);
        spent.after = (spent.ended - end).max(0);
    }
    spent.critical_path = path;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: u64, step: &str, requested: i64, ran: (i64, i64), before: &[u64]) -> Timed {
        Timed {
            id,
            name: format!("{step} ({id})"),
            step: step.into(),
            requested,
            attempts: vec![Span {
                started: ran.0,
                ended: Some(ran.1),
                ..Span::default()
            }],
            before: before.to_vec(),
            trigger: None,
        }
    }

    #[test]
    fn follows_latest_end() {
        // A (0-10) and B (0-30) feed C, which the workflow asks for when B ends and which
        // waits 5 for a free slot.
        let mut c = task(3, "C", 32, (37, 50), &[1, 2]);
        c.trigger = Some(2);
        let tasks = [
            task(1, "A", 1, (2, 10), &[]),
            task(2, "B", 1, (2, 30), &[]),
            c,
        ];
        let spent = spent(&tasks, 0, 52);
        let path: Vec<(u64, i64, i64, i64)> = (spent.critical_path.iter())
            .map(|task| (task.task, task.gap, task.waited, task.ran))
            .collect();
        assert_eq!(path, [(2, 0, 1, 28), (3, 2, 5, 13)]);
        assert_eq!((spent.before, spent.after), (1, 2));
        let parts: i64 = (spent.critical_path.iter())
            .map(|task| task.gap + task.waited + task.ran)
            .sum();
        assert_eq!(
            spent.before + parts + spent.after,
            spent.ended - spent.started
        );
        assert_eq!(spent.ran, 8 + 28 + 13);
        assert_eq!(spent.waited, 1 + 1 + 5);
        assert_eq!(spent.busy, 41);
    }

    #[test]
    fn follows_request_trigger() {
        // D reads nothing of C, but the workflow asked for it only when C ended.
        let mut d = task(4, "D", 21, (21, 25), &[]);
        d.trigger = Some(3);
        let tasks = [
            task(3, "C", 0, (0, 20), &[]),
            d,
            task(5, "E", 0, (0, 5), &[]),
        ];
        let spent = spent(&tasks, 0, 25);
        let path: Vec<u64> = spent.critical_path.iter().map(|task| task.task).collect();
        assert_eq!(path, [3, 4]);
        assert_eq!(spent.critical_path[1].gap, 1);
    }

    #[test]
    fn follows_waited_task() {
        // G was asked for at once but comes after F.
        let tasks = [
            task(6, "F", 0, (0, 40), &[]),
            task(7, "G", 0, (41, 60), &[6]),
        ];
        let spent = spent(&tasks, 0, 60);
        let path: Vec<(u64, i64, i64)> = (spent.critical_path.iter())
            .map(|task| (task.task, task.gap, task.waited))
            .collect();
        assert_eq!(path, [(6, 0, 0), (7, 0, 1)]);
    }

    #[test]
    fn sums_step_usage() {
        let mut a = task(1, "ALIGN", 0, (0, 10), &[]);
        a.attempts[0].cpu_usec = Some(4_000);
        a.attempts[0].memory_peak = Some(100);
        a.attempts[0].read_bytes = Some(7);
        let mut b = task(2, "ALIGN", 0, (0, 20), &[]);
        b.attempts[0].cpu_usec = Some(1_000);
        b.attempts[0].memory_peak = Some(300);
        let mut c = task(3, "QC", 0, (0, 1), &[]);
        c.attempts[0].reused = Some("cache".into());
        let spent = spent(&[a, b, c], 0, 20);
        assert_eq!(spent.reused, 1);
        let align = &spent.steps[0];
        assert_eq!((align.tasks, align.ran), (2, 30));
        assert_eq!(align.cpu_usec, Some(5_000));
        assert_eq!(align.memory_peak, Some(300));
        assert_eq!((align.read_bytes, align.write_bytes), (Some(7), None));
        let qc = &spent.steps[1];
        assert_eq!((qc.tasks, qc.ran, qc.cpu_usec), (1, 0, None));
    }

    #[test]
    fn attributes_empty_runtime() {
        let spent = spent(&[], 5, 9);
        assert!(spent.critical_path.is_empty());
        assert_eq!((spent.before, spent.after, spent.busy), (4, 0, 0));
    }

    #[test]
    fn counts_overlap_once() {
        assert_eq!(union(vec![(0, 5), (3, 8), (10, 12)]), 10);
        assert_eq!(union(Vec::new()), 0);
    }
}
