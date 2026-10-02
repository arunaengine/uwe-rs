//! Compares run settings and pairs tasks to describe execution and content differences.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod pairing;
mod tasks;

use std::collections::{BTreeMap, HashMap, HashSet};

use rusqlite::OptionalExtension;
use serde::Serialize;
use workflow_graph::Task;

use crate::{Database, DatabaseError, FileRow, Run};

/// What differs between two runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Comparison {
    pub first: Run,
    pub second: Run,
    /// What differs between the runs themselves, one sentence each.
    pub changes: Vec<String>,
    /// Every task of both runs: the pairs in the order of the first run, then the tasks that
    /// only the second run has.
    pub tasks: Vec<TaskPair>,
}

/// How a task of one run compares with the same task of the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pairing {
    /// Both runs have the task, and it does the same and made the same outputs.
    Same,
    /// The task does something else: another image, command, script, environment or input.
    Changed,
    /// The task does the same, but some of its outputs have other content.
    Outputs,
    /// Only the first run has the task.
    First,
    /// Only the second run has the task.
    Second,
}

/// A task of the first run and the same task of the second, and what differs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TaskPair {
    pub step: String,
    pub pairing: Pairing,
    pub first: Option<Side>,
    pub second: Option<Side>,
    /// What the task does differently, one sentence each.
    pub changes: Vec<String>,
    /// The outputs whose content differs, or that only one run has.
    pub outputs: Vec<String>,
    /// How the task ended differently: its state, or where its result came from.
    pub results: Vec<String>,
}

/// A task in one of the runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Side {
    pub task: u64,
    pub name: String,
    pub state: String,
    pub exit_code: Option<i32>,
    /// Where its result came from when it did not run: `cache` or an earlier run.
    pub reused: Option<String>,
    /// The image as the task ran in it.
    pub image: Option<String>,
}

/// What the comparison knows of the inputs of a task, by the path the task sees them at.
#[derive(Default)]
struct KnownInputs {
    /// The digest of each input where it is known.
    digests: HashMap<String, String>,
    /// The inputs that a task made. The same task of the other run reads the file of its own
    /// run, at another place, so their places do not tell whether they differ.
    made: HashSet<String>,
}

/// A task of a run with what the comparison needs.
struct Loaded {
    side: Side,
    step: String,
    fingerprint: String,
    description: Task,
    inputs: Vec<FileRow>,
    outputs: Vec<FileRow>,
}

impl Database {
    /// What differs between two runs; `None` when either is unknown.
    pub fn compare(&self, first: &str, second: &str) -> Result<Option<Comparison>, DatabaseError> {
        let (Some(first_run), Some(second_run)) = (self.run(first)?, self.run(second)?) else {
            return Ok(None);
        };
        let mut changes = run_changes(&first_run, &second_run);
        changes.extend(self.input_changes(&first_run, &second_run)?);
        let (earlier, later) = (self.loaded(first)?, self.loaded(second)?);
        let (earlier_outputs, later_outputs) = (self.outputs(first)?, self.outputs(second)?);
        let mut tasks = Vec::new();
        for (a, b) in pairing::paired(&earlier, &later) {
            let pair = match (a.map(|a| &earlier[a]), b.map(|b| &later[b])) {
                (Some(a), Some(b)) => {
                    let a_inputs = self.known_inputs(a, &earlier_outputs)?;
                    let b_inputs = self.known_inputs(b, &later_outputs)?;
                    tasks::compared(a, b, &a_inputs, &b_inputs)
                }
                (Some(a), None) => tasks::alone(a, Pairing::First),
                (None, Some(b)) => tasks::alone(b, Pairing::Second),
                (None, None) => continue,
            };
            tasks.push(pair);
        }
        Ok(Some(Comparison {
            first: first_run,
            second: second_run,
            changes,
            tasks,
        }))
    }

    /// The latest run of the same workflow that started before this one, to compare it with;
    /// for a replay of one task, the run it replays.
    pub fn run_before(&self, run: &str) -> Result<Option<String>, DatabaseError> {
        let found = self
            .connection
            .query_row(
                "SELECT e.id FROM runs r JOIN runs e ON e.workflow = r.workflow
                 WHERE r.id = ?1
                    AND (e.id = r.replays OR r.replays IS NULL AND e.replays IS NULL
                        AND (e.started < r.started OR (e.started = r.started AND e.rowid < r.rowid)))
                 ORDER BY e.id = r.replays DESC, e.started DESC, e.rowid DESC LIMIT 1",
                [run],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found)
    }

    /// The tasks of a run with their last attempt, inputs and the outputs of that attempt.
    fn loaded(&self, run: &str) -> Result<Vec<Loaded>, DatabaseError> {
        let mut statement = self.connection.prepare(
            "SELECT t.id, t.name, t.step, t.fingerprint, t.state, t.description, a.exit_code,
                a.reused, a.image, a.number
             FROM tasks t LEFT JOIN attempts a ON a.run = t.run AND a.task = t.id
                AND a.number = (SELECT MAX(number) FROM attempts WHERE run = t.run AND task = t.id)
             WHERE t.run = ?1 ORDER BY t.rowid",
        )?;
        let rows = statement
            .query_map([run], |row| {
                let side = Side {
                    task: row.get(0)?,
                    name: row.get(1)?,
                    state: row.get(4)?,
                    exit_code: row.get(6)?,
                    reused: row.get(7)?,
                    image: row.get(8)?,
                };
                let texts: (String, String, String) = (row.get(2)?, row.get(3)?, row.get(5)?);
                Ok((side, texts, row.get::<_, Option<u32>>(9)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut files: HashMap<u64, Vec<FileRow>> = HashMap::new();
        for file in self.files(run, None)? {
            files.entry(file.task).or_default().push(file);
        }
        let mut tasks = Vec::with_capacity(rows.len());
        for (side, (step, fingerprint, description), last) in rows {
            let (inputs, outputs): (Vec<FileRow>, Vec<FileRow>) = (files.remove(&side.task))
                .unwrap_or_default()
                .into_iter()
                .partition(|file| file.attempt.is_none());
            tasks.push(Loaded {
                description: serde_json::from_str(&description)?,
                outputs: (outputs.into_iter())
                    .filter(|file| file.attempt == last)
                    .collect(),
                inputs,
                side,
                step,
                fingerprint,
            });
        }
        Ok(tasks)
    }

    /// The places of the outputs of a run, with their digests where known.
    fn outputs(&self, run: &str) -> Result<HashMap<String, Option<String>>, DatabaseError> {
        let mut statement = self
            .connection
            .prepare("SELECT location, digest FROM files WHERE run = ?1 AND role = 'output'")?;
        let outputs = statement
            .query_map([run], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<HashMap<String, Option<String>>, _>>()?;
        Ok(outputs)
    }

    /// What is known of the inputs of a task: the digest of each where it is known (its own,
    /// or that of the output of this run, or of any run, at its place), and which a task made.
    fn known_inputs(
        &self,
        task: &Loaded,
        outputs: &HashMap<String, Option<String>>,
    ) -> Result<KnownInputs, DatabaseError> {
        let mut known = KnownInputs::default();
        for input in &task.inputs {
            if input.location.is_empty() {
                continue;
            }
            let made = match outputs.get(&input.location) {
                Some(digest) => Some(digest.clone()),
                None => self.made_at(&input.location)?,
            };
            if made.is_some() {
                known.made.insert(input.path.clone());
            }
            if let Some(digest) = input.digest.clone().or(made.flatten()) {
                known.digests.insert(input.path.clone(), digest);
            }
        }
        Ok(known)
    }

    /// Whether a task of any run made a file at this place, and its digest where known.
    fn made_at(&self, location: &str) -> Result<Option<Option<String>>, DatabaseError> {
        let made = self
            .connection
            .prepare_cached(
                "SELECT digest FROM files WHERE location = ?1 AND role = 'output'
                 ORDER BY digest IS NULL LIMIT 1",
            )?
            .query_row([location], |row| row.get(0))
            .optional()?;
        Ok(made)
    }

    /// The input values of the runs (decision 24) that differ, beyond their parameters.
    fn input_changes(&self, first: &Run, second: &Run) -> Result<Vec<String>, DatabaseError> {
        let (Some(a), Some(b)) = (self.run_inputs(&first.id)?, self.run_inputs(&second.id)?) else {
            return Ok(Vec::new());
        };
        let parameters: HashSet<&str> = (first.parameters.iter())
            .chain(&second.parameters)
            .map(|(name, _)| name.as_str())
            .collect();
        let object = |value: &serde_json::Value| -> BTreeMap<String, serde_json::Value> {
            match value {
                serde_json::Value::Object(map) => map.clone().into_iter().collect(),
                _ => BTreeMap::new(),
            }
        };
        let (a, b) = (object(&a.values), object(&b.values));
        let mut changes = Vec::new();
        for name in a
            .keys()
            .chain(b.keys().filter(|name| !a.contains_key(*name)))
        {
            if parameters.contains(name.as_str()) {
                continue;
            }
            match (a.get(name), b.get(name)) {
                (Some(x), Some(y)) if x != y => changes.push(format!(
                    "the input `{name}` is {} in the first run and {} in the second",
                    shown(x),
                    shown(y)
                )),
                (Some(x), None) => changes.push(format!(
                    "the input `{name}` is {} in the first run and not set in the second",
                    shown(x)
                )),
                (None, Some(y)) => changes.push(format!(
                    "the input `{name}` is not set in the first run and is {} in the second",
                    shown(y)
                )),
                _ => {}
            }
        }
        Ok(changes)
    }
}

/// What differs between the runs themselves, besides their input values.
fn run_changes(first: &Run, second: &Run) -> Vec<String> {
    let mut changes = Vec::new();
    let mut differ = |what: &str, a: &str, b: &str| {
        if a != b {
            changes.push(format!(
                "the {what} is {a} in the first run and {b} in the second"
            ));
        }
    };
    differ("workflow", &first.workflow, &second.workflow);
    differ("language", &first.language, &second.language);
    differ("executor", &first.executor, &second.executor);
    let revision = |run: &Run| run.revision.clone().unwrap_or_else(|| "unknown".into());
    differ("Git revision", &revision(first), &revision(second));
    if first.simulated != second.simulated {
        let simulated = if first.simulated { "first" } else { "second" };
        changes.push(format!(
            "the {simulated} run was simulated, the other ran for real"
        ));
    }
    let a: BTreeMap<&str, &str> = (first.parameters.iter())
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let b: BTreeMap<&str, &str> = (second.parameters.iter())
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    for (name, value) in &a {
        match b.get(name) {
            Some(other) if other == value => {}
            Some(other) => changes.push(format!(
                "the parameter `{name}` is `{value}` in the first run and `{other}` in the second"
            )),
            None => changes.push(format!(
                "the parameter `{name}` is `{value}` in the first run and not given in the second"
            )),
        }
    }
    for (name, value) in b.iter().filter(|(name, _)| !a.contains_key(*name)) {
        changes.push(format!(
            "the parameter `{name}` is not given in the first run and is `{value}` in the second"
        ));
    }
    changes
}

/// A JSON value in a sentence, cut when it is long.
fn shown(value: &serde_json::Value) -> String {
    let text = value.to_string();
    match text.char_indices().nth(80) {
        Some((cut, _)) => format!("`{}...`", &text[..cut]),
        None => format!("`{text}`"),
    }
}
