//! Runs the CWL document of a `cwl:` rule with the CWL runtime of iunctus, with the input
//! object that Snakemake writes for cwltool.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::Path;

use python_evaluator::{Interpreter, Value};
use serde_json::{Map, Value as Json};
use workflow_graph::{Delivery, Driver, DriverError, Outcome, Task, TaskId};

use crate::objects::NamedList;

/// The CWL document of a job and its input object.
#[derive(Clone, Debug)]
pub struct Job {
    /// A path or an address, as the CWL runtime takes it.
    pub document: String,
    pub inputs: Json,
}

/// The CWL document of a rule, as Snakemake's `cwl()` finds it: an address stays, a path
/// is taken from the folder of the Snakefile.
pub fn document(path: &str, basedir: &Path) -> String {
    if path.starts_with("http") {
        return path.to_owned();
    }
    let path = (path.strip_prefix("file://"))
        .or_else(|| path.strip_prefix("file:"))
        .unwrap_or(path);
    let path = match Path::new(path).is_absolute() {
        true => Path::new(path).to_path_buf(),
        false => python_evaluator::host::normalize(&basedir.join(path)),
    };
    path.to_string_lossy().into_owned()
}

/// The input object of Snakemake's `cwl()`: each named input as a `File` with its absolute
/// path, then the named params, outputs and logs as they are.
pub fn inputs(
    interpreter: &mut Interpreter,
    workdir: &Path,
    lists: [&Value; 4],
) -> Result<Json, python_evaluator::Error> {
    let [input, params, output, log] = lists;
    let mut object = Map::new();
    for (name, value) in named(input) {
        object.insert(name, file_spec(interpreter, workdir, &value)?);
    }
    for list in [params, output, log] {
        for (name, value) in named(list) {
            object.insert(name, json(interpreter, &value)?);
        }
    }
    Ok(Json::Object(object))
}

/// The named items of a named list, a list for a name of many.
fn named(value: &Value) -> Vec<(String, Value)> {
    let Some(list) = value.as_native::<NamedList>() else {
        return Vec::new();
    };
    (list.names.iter())
        .map(|(name, start, end)| {
            let item = match end {
                Some(end) => Value::list(list.values[*start..*end].to_vec()),
                None => list.values[*start].clone(),
            };
            (name.clone(), item)
        })
        .collect()
}

fn file_spec(
    interpreter: &mut Interpreter,
    workdir: &Path,
    value: &Value,
) -> Result<Json, python_evaluator::Error> {
    if let Some(path) = value.as_str() {
        let path = python_evaluator::host::normalize(&workdir.join(path));
        return Ok(serde_json::json!({"path": path.to_string_lossy(), "class": "File"}));
    }
    let items = interpreter.iterate(value)?;
    let mut specs = Vec::with_capacity(items.len());
    for item in items {
        specs.push(file_spec(interpreter, workdir, &item)?);
    }
    Ok(Json::Array(specs))
}

/// A value as `json.dump` writes it.
fn json(interpreter: &mut Interpreter, value: &Value) -> Result<Json, python_evaluator::Error> {
    Ok(match value.plain() {
        Value::None => Json::Null,
        Value::Bool(flag) => Json::Bool(*flag),
        Value::Int(number) => Json::from(*number),
        Value::Float(number) => {
            serde_json::Number::from_f64(*number).map_or(Json::Null, Json::Number)
        }
        Value::Str(text) => Json::String(text.to_string()),
        Value::Dict(_) => {
            let mut object = Map::new();
            for (key, item) in interpreter.mapping_items(value)? {
                object.insert(interpreter.to_str(&key)?, json(interpreter, &item)?);
            }
            Json::Object(object)
        }
        Value::List(_) | Value::Tuple(_) => {
            let mut items = Vec::new();
            for item in interpreter.iterate(value)? {
                items.push(json(interpreter, &item)?);
            }
            Json::Array(items)
        }
        _ => Json::String(interpreter.to_str(value)?),
    })
}

/// The CWL run of one job: its driver, and the tasks it asked for by the numbers the
/// Snakemake run gave them.
pub struct Run {
    pub driver: Box<dyn Driver>,
    /// The task of the job in the Snakemake run, which ends once the CWL run did.
    pub task: TaskId,
    ids: HashMap<TaskId, TaskId>,
}

impl Run {
    pub fn new(driver: Box<dyn Driver>, task: TaskId) -> Run {
        Run {
            driver,
            task,
            ids: HashMap::new(),
        }
    }

    /// Gives the tasks of the CWL run numbers of the Snakemake run, from `next`.
    pub fn number(&mut self, tasks: Vec<Task>, next: &dyn Fn() -> TaskId) -> Vec<Task> {
        let mut numbered = Vec::with_capacity(tasks.len());
        for mut task in tasks {
            let outer = next();
            self.ids.insert(outer, task.id);
            task.id = outer;
            numbered.push(task);
        }
        for task in &mut numbered {
            for after in &mut task.after {
                if let Some((outer, _)) = self.ids.iter().find(|(_, inner)| *inner == after) {
                    *after = *outer;
                }
            }
        }
        numbered
    }

    /// Hands an outcome to the CWL run under its own number of the task; the tasks it asks
    /// for next, and whether it accepted the outcome.
    pub fn complete(&mut self, mut outcome: Outcome) -> Result<(Vec<Task>, bool), DriverError> {
        if let Some(inner) = self.ids.remove(&outcome.id) {
            outcome.id = inner;
        }
        let (inner, attempt) = (outcome.id, outcome.attempt);
        let tasks = self.driver.complete(outcome)?;
        Ok((tasks, self.driver.take_acceptance(inner, attempt)))
    }

    /// Whether the CWL run waits for none of its tasks.
    pub fn idle(&self) -> bool {
        self.ids.is_empty()
    }

    /// Ends the CWL run: the files of its output object, delivered to the working directory
    /// as cwltool leaves them there.
    pub fn finish(&mut self) -> Result<Vec<Delivery>, DriverError> {
        self.driver.finish()?;
        Ok(self.driver.deliveries())
    }
}
