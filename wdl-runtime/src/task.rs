//! Turns a call of a WDL task into a container task, and its outcome into call outputs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use indexmap::IndexMap;
use wdl_analysis::Document;
use wdl_analysis::types::{Optional, Type};
use wdl_ast::AstToken;
use wdl_ast::v1::{Decl, Expr, TaskDefinition};
use workflow_graph::{Files, Input, Resources, Task, TaskId};

use crate::document::declared;
use crate::expression::{self, Environment, Error, names};
use crate::library::memory_bytes;
use crate::value::Value;

#[path = "outputs.rs"]
mod outputs;

pub use outputs::finish;
use outputs::plan_output;

/// The default image of a task without a container, as for CWL tools.
pub const DEFAULT_IMAGE: &str = "debian:stable-slim";

/// Where a task keeps its script and the streams of its command, apart from its outputs.
const SCRIPT: &str = ".wdl/command.sh";
const STDOUT: &str = ".wdl/stdout";
const STDERR: &str = ".wdl/stderr";

/// Runs the script with its streams in files for `stdout()` and `stderr()`, and shows them.
const RUNNER: &str = "mkdir -p .wdl\nbash .wdl/command.sh > .wdl/stdout 2> .wdl/stderr\nstatus=$?\ncat .wdl/stdout\ncat .wdl/stderr >&2\nexit $status\n";

/// What a run gives every task.
#[derive(Clone)]
pub struct Settings {
    pub image: String,
    pub simulated: bool,
    pub files: Arc<dyn Files>,
}

/// A task that was asked for, with what it needs to read its outputs once it ran.
pub struct Planned {
    pub task: Task,
    pub state: State,
}

pub struct State {
    document: Document,
    definition: TaskDefinition,
    /// The inputs and declarations of the task, with files at the paths the task sees.
    names: BTreeMap<String, Value>,
    /// Where each staged file comes from, by the path the task sees it at.
    staged: BTreeMap<String, String>,
    /// The exit codes that count as success; `None` takes every code.
    codes: Option<Vec<i64>>,
}

/// Plans one call of a task with the values of its inputs.
pub fn plan(
    document: &Document,
    definition: &TaskDefinition,
    given: BTreeMap<String, Value>,
    (id, name, step): (TaskId, String, String),
    settings: &Settings,
) -> Result<Planned, Error> {
    let mut frame = Frame {
        names: BTreeMap::new(),
        staged: BTreeMap::new(),
        written: Vec::new(),
        settings: settings.clone(),
    };
    let mut pending: Vec<(String, Type, Expr)> = Vec::new();
    let mut environment = BTreeMap::new();
    let mut exported = BTreeSet::new();
    let inputs: Vec<Decl> = definition
        .input()
        .map(|section| section.declarations().collect())
        .unwrap_or_default();
    for decl in &inputs {
        let name = decl.name().text().to_owned();
        let ty = declared(document, &decl.ty()).map_err(Error::Failed)?;
        if decl.env().is_some() {
            exported.insert(name.clone());
        }
        match (given.get(&name), decl) {
            (Some(value), _) => {
                let value = frame.localize(value.clone().coerce(&ty).map_err(Error::Failed)?);
                frame.names.insert(name, value);
            }
            (None, Decl::Bound(bound)) => pending.push((name, ty, bound.expr())),
            (None, Decl::Unbound(_)) if ty.is_optional() => {
                frame.names.insert(name, Value::None);
            }
            (None, Decl::Unbound(_)) => {
                return Err(Error::failed(format!(
                    "the input `{name}` of task {step} has no value"
                )));
            }
        }
    }
    for decl in definition.declarations() {
        let name = decl.name().text().to_owned();
        if decl.env().is_some() {
            exported.insert(name.clone());
        }
        let ty = declared(document, &decl.ty()).map_err(Error::Failed)?;
        pending.push((name, ty, decl.expr()));
    }
    frame.settle(pending)?;
    for name in exported {
        if let Some(text) = frame.names.get(&name).and_then(Value::text) {
            environment.insert(name, text);
        }
    }
    let runtime = runtime(definition, &mut frame)?;
    let image = match runtime.get("container").or(runtime.get("docker")) {
        Some(Value::String(image)) if image != "*" => image.clone(),
        Some(Value::Array(images)) => match images.first().and_then(Value::text) {
            Some(image) if image != "*" => image,
            _ => settings.image.clone(),
        },
        _ => settings.image.clone(),
    };
    let resources = Resources {
        cpus: match runtime.get("cpu") {
            Some(Value::Int(count)) => Some(*count as f64),
            Some(Value::Float(count)) => Some(*count),
            Some(Value::String(count)) => count.parse().ok(),
            _ => None,
        },
        memory_bytes: runtime.get("memory").and_then(memory_bytes),
        gpus: matches!(runtime.get("gpu"), Some(Value::Boolean(true))).then_some(1),
        ..Resources::default()
    };
    let codes = match runtime.get("return_codes").or(runtime.get("returnCodes")) {
        None => Some(vec![0]),
        Some(Value::Int(code)) => Some(vec![*code]),
        Some(Value::String(any)) if any == "*" => None,
        Some(Value::Array(codes)) => Some(
            (codes.iter())
                .map(|code| match code {
                    Value::Int(code) => Ok(*code),
                    _ => Err(Error::failed("return codes must be integers")),
                })
                .collect::<Result<_, _>>()?,
        ),
        Some(other) => {
            return Err(Error::failed(format!(
                "return codes cannot be a {}",
                other.kind()
            )));
        }
    };
    frame.names.insert(
        "task".into(),
        task_object(definition, &image, &resources, None),
    );
    let script = match definition.command() {
        Some(section) => expression::command(&section, &mut frame)?,
        None => String::new(),
    };
    let mut patterns = BTreeSet::new();
    let mut planned = BTreeMap::new();
    if let Some(section) = definition.output() {
        for decl in section.declarations() {
            plan_output(document, &decl, &mut frame, &mut planned, &mut patterns)?;
        }
    }
    let mut inputs: Vec<Input> = vec![Input {
        path: SCRIPT.into(),
        content: Some(script.clone()),
        ..Input::default()
    }];
    for (path, content) in &frame.written {
        inputs.push(Input {
            path: path.clone(),
            content: Some(content.clone()),
            ..Input::default()
        });
    }
    for (path, source) in &frame.staged {
        inputs.push(Input {
            path: path.clone(),
            source: source.clone(),
            directory: path.ends_with('/'),
            ..Input::default()
        });
    }
    for input in &mut inputs {
        input.path = input.path.trim_end_matches('/').to_owned();
    }
    let task = Task {
        id,
        name,
        step,
        image: Some(image),
        command: vec!["bash".into(), "-c".into(), RUNNER.into()],
        script: Some(script),
        environment,
        inputs,
        outputs: patterns.into_iter().collect(),
        resources,
        ..Task::default()
    };
    let state = State {
        document: document.clone(),
        definition: definition.clone(),
        names: frame.names,
        staged: frame.staged,
        codes,
    };
    Ok(Planned { task, state })
}

/// The values of a task's `runtime` and `requirements` sections, by key.
fn runtime(
    definition: &TaskDefinition,
    frame: &mut Frame,
) -> Result<BTreeMap<String, Value>, Error> {
    let mut values = BTreeMap::new();
    let items: Vec<(String, Expr)> = match (definition.requirements(), definition.runtime()) {
        (Some(section), _) => section
            .items()
            .map(|item| (item.name().text().to_owned(), item.expr()))
            .collect(),
        (None, Some(section)) => section
            .items()
            .map(|item| (item.name().text().to_owned(), item.expr()))
            .collect(),
        (None, None) => Vec::new(),
    };
    for (key, expr) in items {
        values.insert(key, expression::evaluate(&expr, frame)?);
    }
    Ok(values)
}

/// The value of the `task` variable of WDL 1.2, before the run and after it.
fn task_object(
    definition: &TaskDefinition,
    image: &str,
    resources: &Resources,
    code: Option<i32>,
) -> Value {
    let mut fields = IndexMap::new();
    fields.insert(
        "name".into(),
        Value::String(definition.name().text().to_owned()),
    );
    fields.insert(
        "id".into(),
        Value::String(definition.name().text().to_owned()),
    );
    fields.insert("attempt".into(), Value::Int(0));
    fields.insert("container".into(), Value::String(image.to_owned()));
    fields.insert("cpu".into(), Value::Float(resources.cpus.unwrap_or(1.0)));
    let memory = resources.memory_bytes.map_or(0, |bytes| bytes as i64);
    fields.insert("memory".into(), Value::Int(memory));
    fields.insert(
        "return_code".into(),
        code.map_or(Value::None, |code| Value::Int(code as i64)),
    );
    Value::Object(fields)
}

/// The names of a task while it is planned, and the files it stages and writes.
struct Frame {
    names: BTreeMap<String, Value>,
    staged: BTreeMap<String, String>,
    written: Vec<(String, String)>,
    settings: Settings,
}

impl Frame {
    /// Evaluates declarations in the order their names allow.
    fn settle(&mut self, mut pending: Vec<(String, Type, Expr)>) -> Result<(), Error> {
        let declared: BTreeSet<String> = pending.iter().map(|(name, _, _)| name.clone()).collect();
        while !pending.is_empty() {
            let ready = pending.iter().position(|(_, _, expr)| {
                (names(expr).iter())
                    .all(|name| !declared.contains(name) || self.names.contains_key(name))
            });
            let Some(ready) = ready else {
                let waiting: Vec<&str> = pending.iter().map(|(name, _, _)| name.as_str()).collect();
                return Err(Error::failed(format!(
                    "these declarations refer to each other: {}",
                    waiting.join(", ")
                )));
            };
            let (name, ty, expr) = pending.remove(ready);
            let value = expression::evaluate(&expr, self)?;
            let value = value
                .coerce(&ty)
                .map_err(|error| Error::failed(format!("`{name}`: {error}")))?;
            let value = self.localize(value);
            self.names.insert(name, value);
        }
        Ok(())
    }

    /// The value with each file and directory at the path where the task sees it.
    fn localize(&mut self, mut value: Value) -> Value {
        value.visit_paths(&mut |path, directory| {
            let outside = path.starts_with('/') || path.contains("://");
            if !outside || self.staged.contains_key(path.as_str()) {
                return;
            }
            let known = self
                .staged
                .iter()
                .find(|(_, source)| *source == path)
                .map(|(inside, _)| inside.clone());
            let inside = known.unwrap_or_else(|| {
                let name = path
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or("input")
                    .to_owned();
                let inside = format!("inputs/{}/{name}", self.staged.len());
                let key = if directory {
                    format!("{inside}/")
                } else {
                    inside.clone()
                };
                self.staged.insert(key, path.clone());
                inside
            });
            *path = inside;
        });
        value
    }

    fn source(&self, file: &str) -> Option<&String> {
        self.staged
            .get(file)
            .or_else(|| self.staged.get(&format!("{file}/")))
    }
}

impl Environment for Frame {
    fn lookup(&self, name: &str) -> Option<Value> {
        self.names.get(name).cloned()
    }

    fn read(&mut self, file: &str) -> Result<String, Error> {
        if let Some((_, content)) = self.written.iter().find(|(path, _)| path == file) {
            return Ok(content.clone());
        }
        let source = self
            .source(file)
            .cloned()
            .unwrap_or_else(|| file.to_owned());
        read_location(&*self.settings.files, &source)
    }

    fn write(&mut self, content: String) -> Result<Value, Error> {
        let path = format!(".wdl/written/{}", self.written.len());
        self.written.push((path.clone(), content));
        Ok(Value::File(path))
    }

    fn size(&mut self, file: &str) -> Result<u64, Error> {
        let source = self
            .source(file)
            .cloned()
            .unwrap_or_else(|| file.to_owned());
        size_location(&*self.settings.files, &source)
    }
}

/// The text of a file at a location: a local path, or a file of the store.
pub fn read_location(files: &dyn Files, location: &str) -> Result<String, Error> {
    let local = files
        .local(location)
        .map_err(|error| Error::failed(format!("{location}: {error}")))?;
    std::fs::read_to_string(&local).map_err(|error| Error::failed(format!("{location}: {error}")))
}

/// The size of a file or directory at a location, in bytes.
pub fn size_location(files: &dyn Files, location: &str) -> Result<u64, Error> {
    fn total(path: &std::path::Path) -> std::io::Result<u64> {
        let metadata = std::fs::metadata(path)?;
        if !metadata.is_dir() {
            return Ok(metadata.len());
        }
        let mut sum = 0;
        for entry in std::fs::read_dir(path)? {
            sum += total(&entry?.path())?;
        }
        Ok(sum)
    }
    let local = files
        .local(location)
        .map_err(|error| Error::failed(format!("{location}: {error}")))?;
    total(&local).map_err(|error| Error::failed(format!("{location}: {error}")))
}
