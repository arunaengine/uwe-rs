//! The workflow outputs of the `output { }` block, as Nextflow's `PublishOp` publishes them:
//! the task files in each value that a published channel gives go below the output
//! directory, into the folder of the output's `path`, or where the publish statements of a
//! `path` closure say. Once the channel ends, an index file lists the values, with the files
//! at their new places.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, OutputOptions};
use nextflow_evaluator::value::{PathValue, Value};
use workflow_graph::{Delivery, DeliveryMode};

use crate::channel::Message;
use crate::network::{Behaviour, Context, State};

/// Where the files of one value go.
enum Target {
    /// A folder; each file keeps its name below it.
    Folder(PathBuf),
    /// Each file by its name, from publish statements.
    Files(HashMap<String, PathBuf>),
}

/// A published channel of a workflow output.
pub(crate) struct Publish {
    pub name: Rc<str>,
    pub options: OutputOptions,
    /// The output directory of the run.
    pub directory: PathBuf,
    pub mode: DeliveryMode,
    /// Whether a file already at the target is replaced.
    pub overwrite: bool,
    /// The values that were published, with the files at their new places, for the index.
    pub published: Vec<Value>,
}

impl Behaviour for Publish {
    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        while let Some(message) = context.read(0) {
            match message {
                Message::Value(value) => self.publish(context, value)?,
                Message::Stop => {
                    self.write_index(context)?;
                    return Ok(State::Stopped);
                }
            }
        }
        Ok(State::Running)
    }
}

impl Publish {
    /// Publishes the task files of one value.
    fn publish(&mut self, context: &mut Context, value: Value) -> Result<(), Error> {
        let target = match self.options.get("path").cloned() {
            None => Target::Folder(self.directory.clone()),
            Some(Value::Closure(closure)) => {
                let closure = Value::Closure(closure);
                let (folder, statements) = context
                    .interpreter
                    .call_output_path(&closure, value.clone())?;
                match statements {
                    Some(statements) => Target::Files(self.statements(context, statements)?),
                    None if matches!(folder, Value::Null) => return Ok(()),
                    None => Target::Folder(self.directory.join(folder.to_string())),
                }
            }
            Some(path) => Target::Folder(self.directory.join(path.to_string())),
        };
        let mut files = Vec::new();
        collect(&value, &mut files);
        let mut deliveries = Vec::new();
        let mut places = HashMap::new();
        for file in files {
            let Some(name) = context.task_file(&file.path) else {
                continue;
            };
            let place = match &target {
                Target::Folder(folder) => folder.join(&name),
                Target::Files(targets) => match targets.get(&name) {
                    Some(place) => place.clone(),
                    None => continue,
                },
            };
            places.insert(file.path.clone(), place.clone());
            if !self.overwrite && place.exists() {
                continue;
            }
            deliveries.push(Delivery {
                location: file.path.clone(),
                target: place.to_string_lossy().into_owned(),
                mode: self.mode,
            });
        }
        context.deliver(deliveries);
        self.published.push(normalized(&value, &places));
        Ok(())
    }

    /// The places of the files of publish statements: a target that ends with `/` is a folder
    /// that the file keeps its name in, other targets name the file.
    fn statements(
        &self,
        context: &Context,
        statements: Vec<(Value, String)>,
    ) -> Result<HashMap<String, PathBuf>, Error> {
        let mut places = HashMap::new();
        for (source, target) in statements {
            let sources = match source {
                Value::Null => continue,
                Value::Path(path) => vec![path],
                Value::List(list) if target.ends_with('/') => (list.borrow().items.iter())
                    .filter_map(|item| match item {
                        Value::Path(path) => Some(path.clone()),
                        _ => None,
                    })
                    .collect(),
                other => {
                    let message = format!(
                        "Invalid publish source for workflow output '{}' -- expected a file or \
                         collection of files, but received: {other}",
                        self.name
                    );
                    return Err(thrown(message));
                }
            };
            for source in sources {
                let Some(name) = context.task_file(&source.path) else {
                    continue;
                };
                let place = match target.ends_with('/') {
                    true => format!("{target}{name}"),
                    false => target.clone(),
                };
                places.insert(name, self.directory.join(place));
            }
        }
        Ok(places)
    }

    /// Writes the index file, when the output has one, as CSV, JSON or YAML by its extension;
    /// a simulated run writes none, as it writes nothing into the folders of the user.
    fn write_index(&self, context: &mut Context) -> Result<(), Error> {
        let Some(path) = self.options.index("path") else {
            return Ok(());
        };
        if context.interpreter.host().simulated() {
            return Ok(());
        }
        let file = self.directory.join(path.to_string());
        let text = match file.extension().and_then(|extension| extension.to_str()) {
            Some("csv") => {
                let header = self.options.index("header").cloned();
                let separator = self
                    .options
                    .index("sep")
                    .map_or(",".into(), Value::to_string);
                csv(&self.published, header.as_ref(), &separator)?
            }
            // JSON is YAML too.
            Some("json" | "yaml" | "yml") => {
                let mut text = String::new();
                json(&Value::list(self.published.clone()), &mut text);
                text + "\n"
            }
            _ => {
                let message = format!(
                    "Invalid extension for index file '{}' -- should be CSV, JSON, or YAML",
                    file.display()
                );
                return Err(thrown(message));
            }
        };
        let path = PathValue::parse(&file.to_string_lossy(), "/");
        context.interpreter.host().write(&path, text.as_bytes())
    }
}

/// The paths in a value: the value itself, or those in its lists and maps.
fn collect(value: &Value, files: &mut Vec<PathValue>) {
    match value {
        Value::Path(path) => files.push(PathValue::clone(path)),
        Value::List(list) => {
            for item in &list.borrow().items {
                collect(item, files);
            }
        }
        Value::Map(map) => {
            for value in map.borrow().entries.values() {
                collect(value, files);
            }
        }
        _ => {}
    }
}

/// A value with its published files at their new places.
fn normalized(value: &Value, places: &HashMap<String, PathBuf>) -> Value {
    match value {
        Value::Path(path) => match places.get(&path.path) {
            Some(place) => Value::Path(Rc::new(PathValue::parse(&place.to_string_lossy(), "/"))),
            None => value.clone(),
        },
        Value::List(list) => Value::list(
            (list.borrow().items.iter())
                .map(|item| normalized(item, places))
                .collect(),
        ),
        Value::Map(map) => Value::map(
            (map.borrow().entries.iter())
                .map(|(key, item)| (key.0.clone(), normalized(item, places)))
                .collect::<Vec<_>>(),
        ),
        other => other.clone(),
    }
}

/// The index as CSV, as Nextflow's `CsvWriter` writes it: every field quoted, a header from
/// the keys of the first record or from the list of names given.
fn csv(records: &[Value], header: Option<&Value>, separator: &str) -> Result<String, Error> {
    let columns: Option<Vec<Value>> = match header {
        Some(Value::List(names)) => Some(names.borrow().items.clone()),
        Some(Value::Boolean(true)) => match records.first() {
            Some(Value::Map(map)) => Some(
                map.borrow()
                    .entries
                    .keys()
                    .map(|key| key.0.clone())
                    .collect(),
            ),
            Some(_) => {
                return Err(thrown(
                    "Records must be map objects when header=true".into(),
                ));
            }
            None => None,
        },
        _ => None,
    };
    let field = |value: &Value| match value {
        Value::Null => String::new(),
        Value::Path(path) => path.uri(),
        other => other.to_string(),
    };
    let line = |values: Vec<String>| {
        let fields: Vec<String> = values.iter().map(|value| format!("\"{value}\"")).collect();
        fields.join(separator) + "\n"
    };
    let mut text = String::new();
    if let Some(columns) = &columns {
        text.push_str(&line(columns.iter().map(Value::to_string).collect()));
    }
    for record in records {
        let values: Vec<String> = match record {
            Value::List(list) => list.borrow().items.iter().map(field).collect(),
            Value::Map(map) => {
                let map = map.borrow();
                match &columns {
                    Some(columns) => (columns.iter())
                        .map(|column| {
                            let key = nextflow_evaluator::value::Key(column.clone());
                            map.entries.get(&key).map_or(String::new(), field)
                        })
                        .collect(),
                    None => map.entries.values().map(field).collect(),
                }
            }
            other => vec![field(other)],
        };
        text.push_str(&line(values));
    }
    Ok(text)
}

/// A value as JSON; paths are their text.
fn json(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Boolean(value) => out.push_str(if *value { "true" } else { "false" }),
        Value::Number(number) => {
            let _ = write!(out, "{number}");
        }
        Value::List(list) => {
            out.push('[');
            for (index, item) in list.borrow().items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                json(item, out);
            }
            out.push(']');
        }
        Value::Map(map) => {
            out.push('{');
            for (index, (key, item)) in map.borrow().entries.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                string(&key.0.to_string(), out);
                out.push(':');
                json(item, out);
            }
            out.push('}');
        }
        Value::Path(path) => string(&path.uri(), out),
        other => string(&other.to_string(), out),
    }
}

fn string(text: &str, out: &mut String) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            character if (character as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", character as u32);
            }
            character => out.push(character),
        }
    }
    out.push('"');
}

fn thrown(message: String) -> Error {
    Error::Thrown {
        exception: Rc::new(Exception::new("ScriptRuntimeException", message)),
        span: Default::default(),
    }
}

/// The output directory of a run: `outputDir` of the config, else `-output-dir`, else
/// `results`, relative to the launch directory.
pub(crate) fn output_directory(configured: Option<&str>, launch: &Path) -> PathBuf {
    let folder = configured.unwrap_or("results");
    launch.join(folder)
}
