//! Compares workflow steps by their inputs, task construction and output digests.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Serialize;
use task_model::Digest;
use workflow_graph::{Resources, Task, TaskId};

use crate::{Database, DatabaseError};

/// The most outputs that a difference names.
const NAMED: usize = 5;

/// A step whose tasks differ between two runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StepDifference {
    pub step: String,
    pub change: StepChange,
    /// The number of tasks of the step in the first run and in the second.
    pub tasks: (usize, usize),
    /// Outputs whose content differs, by path, at most a few.
    pub outputs: Vec<String>,
    /// For a step whose inputs differ: the steps that made its inputs and differ too.
    pub after: Vec<String>,
}

/// How the tasks of a step differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepChange {
    /// The runs have another number of tasks of the step.
    Tasks,
    /// The tasks read the same content, but are made another way: another command, script,
    /// environment or order of their inputs.
    Made,
    /// The tasks read other content, which other steps made.
    Inputs,
    /// The tasks are the same and read the same content, but made other bytes.
    Outputs,
}

/// What the tasks of a step of one run are, each list sorted, so that runs compare without
/// pairs of tasks.
#[derive(Default)]
struct Step {
    /// How each task is made, without its id, name, resources and where its inputs are.
    made: Vec<String>,
    /// The content of the inputs of each task, whatever their paths.
    inputs: Vec<String>,
    /// The outputs of each task by path and digest.
    outputs: Vec<Vec<(String, String)>>,
    /// The steps that made its inputs.
    after: BTreeSet<String>,
}

impl Database {
    /// The steps whose tasks differ between two runs, in the order the first run asked for
    /// them, then the steps that only the second run has.
    pub fn compare_steps(
        &self,
        first: &str,
        second: &str,
    ) -> Result<Vec<StepDifference>, DatabaseError> {
        let (order, a) = self.steps(first)?;
        let (more, b) = self.steps(second)?;
        let mut names = order;
        names.extend(more.into_iter().filter(|name| !a.contains_key(name)));
        let empty = Step::default();
        let mut differences: Vec<StepDifference> = Vec::new();
        for name in names {
            let (x, y) = (
                a.get(&name).unwrap_or(&empty),
                b.get(&name).unwrap_or(&empty),
            );
            let change = if x.made.len() != y.made.len() {
                StepChange::Tasks
            } else if x.outputs == y.outputs && x.made == y.made {
                continue;
            } else if x.inputs != y.inputs {
                StepChange::Inputs
            } else if x.made != y.made {
                StepChange::Made
            } else {
                StepChange::Outputs
            };
            differences.push(StepDifference {
                outputs: differing(&x.outputs, &y.outputs),
                tasks: (x.made.len(), y.made.len()),
                after: Vec::new(),
                step: name,
                change,
            });
        }
        // A step whose inputs differ follows from the steps that made them and differ.
        let differ: BTreeSet<String> = (differences.iter())
            .map(|difference| difference.step.clone())
            .collect();
        for difference in &mut differences {
            if difference.change == StepChange::Inputs
                && let Some(step) = a.get(&difference.step)
            {
                difference.after = (step.after.iter())
                    .filter(|earlier| differ.contains(*earlier) && **earlier != difference.step)
                    .cloned()
                    .collect();
            }
        }
        Ok(differences)
    }

    /// The steps of a run by name, and their names in the order the run asked for them.
    fn steps(&self, run: &str) -> Result<(Vec<String>, HashMap<String, Step>), DatabaseError> {
        let tasks = self.tasks(run)?;
        let files = self.files(run, None)?;
        // The outputs of the tasks, by where they are: the content of the inputs of later
        // tasks, and which step made them.
        let mut made_at: HashMap<&str, (&str, u64)> = HashMap::new();
        let step_of: HashMap<u64, &str> = (tasks.iter())
            .map(|task| (task.id, task.step.as_str()))
            .collect();
        for file in files.iter().filter(|file| file.role == "output") {
            let digest = file.digest.as_deref().unwrap_or("unknown");
            made_at.insert(&file.location, (digest, file.task));
        }
        let mut order: Vec<String> = Vec::new();
        let mut steps: HashMap<String, Step> = HashMap::new();
        let mut inputs: HashMap<u64, Vec<String>> = HashMap::new();
        // The content at each path of each task, in the order of its inputs.
        let mut placed: HashMap<u64, Vec<String>> = HashMap::new();
        let mut outputs: HashMap<u64, Vec<(String, String)>> = HashMap::new();
        let mut after: HashMap<u64, BTreeSet<String>> = HashMap::new();
        let last = self.last_attempts(run)?;
        for file in &files {
            match file.attempt {
                // What the workflow writes itself, like a task script, is part of how the task
                // is made.
                None if file.location.is_empty() => {}
                None => {
                    let content = match (&file.digest, made_at.get(file.location.as_str())) {
                        (Some(digest), _) => digest.clone(),
                        (None, Some((digest, _))) => (*digest).to_owned(),
                        (None, None) => format!("{} {:?}", file.location, file.size),
                    };
                    placed
                        .entry(file.task)
                        .or_default()
                        .push(format!("{} {content}", file.path));
                    inputs.entry(file.task).or_default().push(content);
                    if let Some((_, maker)) = made_at.get(file.location.as_str())
                        && let Some(step) = step_of.get(maker)
                    {
                        after
                            .entry(file.task)
                            .or_default()
                            .insert((*step).to_owned());
                    }
                }
                Some(attempt) if last.get(&file.task) == Some(&attempt) => {
                    let digest = file.digest.clone().unwrap_or_else(|| "unknown".into());
                    outputs
                        .entry(file.task)
                        .or_default()
                        .push((file.path.clone(), digest));
                }
                Some(_) => {}
            }
        }
        let descriptions = self.descriptions(run)?;
        for task in &tasks {
            let Some(description) = descriptions.get(&task.id) else {
                continue;
            };
            let step = steps.entry(task.step.clone()).or_insert_with(|| {
                order.push(task.step.clone());
                Step::default()
            });
            let paths = placed.remove(&task.id).unwrap_or_default();
            step.made.push(made(description, &paths, run));
            let mut read = inputs.remove(&task.id).unwrap_or_default();
            read.retain(|content| !content.is_empty());
            read.sort();
            step.inputs.push(read.join("\n"));
            let mut made_files = outputs.remove(&task.id).unwrap_or_default();
            made_files.sort();
            step.outputs.push(made_files);
            step.after
                .extend(after.remove(&task.id).unwrap_or_default());
        }
        for step in steps.values_mut() {
            step.made.sort();
            step.inputs.sort();
            step.outputs.sort();
        }
        Ok((order, steps))
    }

    /// The description of each task of a run.
    fn descriptions(&self, run: &str) -> Result<HashMap<u64, Task>, DatabaseError> {
        let mut statement = self
            .connection
            .prepare("SELECT id, description FROM tasks WHERE run = ?1")?;
        let rows = statement
            .query_map([run], |row| Ok((row.get(0)?, row.get::<_, String>(1)?)))?
            .collect::<Result<Vec<(u64, String)>, _>>()?;
        rows.into_iter()
            .map(|(id, text)| Ok((id, serde_json::from_str(&text)?)))
            .collect()
    }

    /// The number of the last attempt of each task of a run.
    fn last_attempts(&self, run: &str) -> Result<HashMap<u64, u32>, DatabaseError> {
        let mut statement = self
            .connection
            .prepare("SELECT task, MAX(number) FROM attempts WHERE run = ?1 GROUP BY task")?;
        let last = statement
            .query_map([run], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<HashMap<u64, u32>, _>>()?;
        Ok(last)
    }
}

/// How a task is made, as a digest: its description without its id, name, resources, the
/// tasks it waits for and where its inputs are, with the id of the run left out of its text,
/// as scratch folders hold it; and which content it sees at each path of its inputs.
fn made(task: &Task, paths: &[String], run: &str) -> String {
    let mut described = Task {
        id: TaskId(0),
        name: String::new(),
        resources: Resources::default(),
        after: Vec::new(),
        ..task.clone()
    };
    for input in &mut described.inputs {
        input.source.clear();
    }
    let text = serde_json::to_string(&described).unwrap_or_default();
    let text = format!("{}\n{}", text.replace(run, "<run>"), paths.join("\n"));
    Digest::sha256(text.as_bytes()).to_string()
}

/// The paths of outputs that one run made with other content than the other, at most
/// [`NAMED`].
fn differing(a: &[Vec<(String, String)>], b: &[Vec<(String, String)>]) -> Vec<String> {
    let count = |outputs: &[Vec<(String, String)>]| {
        let mut counted: BTreeMap<(String, String), usize> = BTreeMap::new();
        for (path, digest) in outputs.iter().flatten() {
            *counted.entry((path.clone(), digest.clone())).or_default() += 1;
        }
        counted
    };
    let (x, y) = (count(a), count(b));
    let mut paths: BTreeSet<String> = BTreeSet::new();
    for (key, number) in x.iter().chain(y.iter()) {
        if x.get(key) != y.get(key) || x.get(key) != Some(number) {
            paths.insert(key.0.clone());
        }
    }
    paths.into_iter().take(NAMED).collect()
}
