//! Runs single template calls of an Argo document as the native run runs a step or task.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;
use std::rc::Rc;

use argo_parser::model::{AnyString, Parameter, Sequence, TemplateRef};
use argo_parser::{Entry, Node, Position, Value as Yaml};
use serde_json::{Map, Value as Json};
use workflow_graph::{DriverError, Files, Launch, Outcome, Task, TaskId};

use crate::items::{item_variables, items};
use crate::node::{ArtifactValue, Facts, Location};
use crate::run::{Args, Call, Holder, Run};
use crate::scope::json_or_text;
use crate::tags::Variables;
use crate::value::Value;
use crate::{Document, Library, Options, Services, artifact_value, driver_error, load_launch};

/// The template calls of one Argo document, for dialect tasks of wir. Each call is a node of
/// one native run, so its tasks, retries, acceptance and outputs are those of the native run.
pub struct Adapter {
    run: Run,
    calls: Vec<usize>,
}

/// A call of a template: its node name, the template and the values of its inputs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Instance {
    /// The node name after the workflow name: `[0].make` for a step, `.make` for a DAG task.
    pub name: String,
    pub template: String,
    /// The WorkflowTemplate that holds the template, and whether it is cluster scoped.
    pub reference: Option<(String, bool)>,
    pub parameters: Vec<(String, String)>,
    /// Files and Directories in the form of wir, by artifact name; null leaves one out.
    pub artifacts: Vec<(String, Json)>,
}

/// What a lowered Argo text gives once its tags are substituted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// The text itself.
    Text,
    /// Whether the text, a `when` condition, holds.
    Condition,
    /// The items of a loop: the text is a JSON list, as `withParam` holds it, or the JSON
    /// object of a `withSequence`.
    Items,
    /// The suffixes that the items of a loop give their node names, like `(0:a)`.
    Labels,
}

/// A started call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallId(usize);

/// How a call ended: its results, its message, and its outputs by native selector:
/// `parameter/<name>` and `result` as texts, `exitCode` as text, `artifact/<name>` as a File
/// or Directory.
#[derive(Clone, Debug, PartialEq)]
pub struct Ending {
    pub facts: Facts,
    pub message: String,
    pub outputs: Map<String, Json>,
}

impl Adapter {
    pub fn new(
        document: Rc<Document>,
        library: Rc<Library>,
        options: Options,
    ) -> Result<Adapter, DriverError> {
        let run = Run::new(document, library, options).map_err(driver_error)?;
        Ok(Adapter {
            run,
            calls: Vec::new(),
        })
    }

    /// The adapter of the document that a launch runs, with the services of its host.
    pub fn load(launch: &Launch, services: Services) -> Result<Adapter, DriverError> {
        let (document, library, options) = load_launch(launch, services)?;
        Adapter::new(document, Rc::new(library), options)
    }

    /// The name of the workflow, which the node names of the calls start with.
    pub fn workflow_name(&self) -> &str {
        &self.run.workflow_name
    }

    pub fn use_files(&mut self, files: std::sync::Arc<dyn Files>) {
        self.run.files = Some(files);
    }

    /// Starts a call, and gives the tasks it asks for first.
    pub fn start(&mut self, instance: &Instance) -> Result<(CallId, Vec<Task>), DriverError> {
        let name = format!("{}{}", self.run.workflow_name, instance.name);
        if self.run.names.contains_key(&name) {
            return Err(DriverError::new(format!("node {name} has started already")));
        }
        let holder = Holder {
            template: instance.template.clone(),
            template_ref: (instance.reference.clone()).map(|(name, cluster_scope)| TemplateRef {
                name,
                template: instance.template.clone(),
                cluster_scope,
            }),
            inline: None,
        };
        let parameters = (instance.parameters.iter())
            .map(|(name, value)| Parameter {
                name: name.clone(),
                value: Some(AnyString(value.clone())),
                ..Parameter::default()
            })
            .collect();
        let mut artifacts = Vec::new();
        for (name, value) in &instance.artifacts {
            artifacts.extend(artifact_of(name, value)?);
        }
        let args = Args {
            parameters,
            artifacts,
        };
        let context = Rc::clone(&self.run.root_context);
        let call = Call::default();
        let index = (self
            .run
            .execute_template(&name, &holder, &context, &args, &call))
        .map_err(driver_error)?;
        self.calls.push(index);
        Ok((CallId(index), self.settle()?))
    }

    /// Takes the outcome of a task of a call, and gives the tasks that its calls ask for next,
    /// like a later attempt.
    pub fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        self.run.record(outcome).map_err(driver_error)?;
        self.settle()
    }

    /// Whether the call accepted that attempt of its task.
    pub fn take_acceptance(&mut self, task: TaskId, attempt: u32) -> bool {
        self.run.take_acceptance(task, attempt)
    }

    /// The results of a call so far; a daemon counts as daemoned once it runs.
    pub fn facts(&self, call: CallId) -> Facts {
        self.run.facts(call.0)
    }

    /// How a call ended, once it did.
    pub fn ending(&self, call: CallId) -> Option<Ending> {
        let node = &self.run.nodes[call.0];
        if !node.phase.fulfilled() {
            return None;
        }
        let mut outputs = Map::new();
        for (name, value) in &node.outputs.parameters {
            outputs.insert(format!("parameter/{name}"), value.as_str().into());
        }
        for artifact in &node.outputs.artifacts {
            let value = artifact_value(artifact);
            outputs.insert(format!("artifact/{}", artifact.name), value);
        }
        if let Some(result) = &node.outputs.result {
            outputs.insert("result".into(), result.as_str().into());
        }
        if let Some(code) = &node.outputs.exit_code {
            outputs.insert("exitCode".into(), code.as_str().into());
        }
        Some(Ending {
            facts: self.run.facts(call.0),
            message: node.message.clone(),
            outputs,
        })
    }

    /// When a call waits for time to pass, like the backoff of a retry.
    pub fn wake_at(&self) -> Option<std::time::Instant> {
        self.run.wake_at()
    }

    /// Goes on after a wait, and gives the tasks that the calls ask for.
    pub fn wake(&mut self, now: std::time::Instant) -> Result<Vec<Task>, DriverError> {
        self.run.clock = self.run.clock.max(now);
        self.settle()
    }

    /// The tasks that the calls stopped, like those past their deadline.
    pub fn cancellations(&mut self) -> Vec<TaskId> {
        self.run.cancellations()
    }

    pub fn warnings(&self) -> &[String] {
        &self.run.warnings
    }

    /// Evaluates an Argo text of the step or task `site`, a node name below the workflow, as
    /// the native run expands it: with the globals of the run and `values` by tag name. A list
    /// is the outputs of a loop, a map an item, and null a value that is not there.
    pub fn evaluate(
        &mut self,
        text: &str,
        form: Form,
        site: &str,
        values: &Map<String, Json>,
    ) -> Result<Json, String> {
        let mut tags = self.run.globals.clone();
        for (name, value) in values {
            tags.extend(tag_values(name, value)?);
        }
        let text = crate::tags::replace(text, &Variables::new(tags), true)?;
        let site = format!("{}{site}", self.run.workflow_name);
        if form == Form::Condition {
            return self.run.decide_when(&text, &site).map(Json::Bool);
        }
        if form == Form::Text {
            return Ok(Json::String(text));
        }
        let mut warnings = Vec::new();
        let list = loop_items(&text, &site, self.run.options.assume_inputs, &mut warnings);
        for warning in warnings {
            self.run.warn(warning);
        }
        let mut found = Vec::new();
        for (index, item) in list?.iter().enumerate() {
            found.push(match form {
                Form::Labels => Json::String(item_variables("", index, item)?.1),
                _ => serde_json::from_str(&item.to_json()).map_err(|error| error.to_string())?,
            });
        }
        Ok(Json::Array(found))
    }

    /// Goes on with every call that runs until none changes, as a pass of the native run does.
    fn settle(&mut self) -> Result<Vec<Task>, DriverError> {
        if !self.run.options.assume_inputs {
            self.run.clock = std::time::Instant::now();
        }
        loop {
            self.run.changed = false;
            self.run.expire();
            for &index in &self.calls {
                if !self.run.nodes[index].phase.fulfilled() {
                    self.run.reenter(index).map_err(driver_error)?;
                }
            }
            if !self.run.changed {
                break;
            }
        }
        Ok(std::mem::take(&mut self.run.emitted))
    }
}

/// The argument of an input artifact from a File or Directory of wir: its location is a path,
/// or an address like `s3://bucket/key`.
fn artifact_of(name: &str, value: &Json) -> Result<Option<ArtifactValue>, DriverError> {
    if value.is_null() {
        return Ok(None);
    }
    let Some(location) = value.get("location").and_then(Json::as_str) else {
        return Err(DriverError::new(format!(
            "artifact {name} needs a File or Directory with a location"
        )));
    };
    let location = match location.strip_prefix("file://") {
        Some(path) => Location::File(PathBuf::from(path)),
        None if location.contains("://") => Location::Url(location.to_owned()),
        None => Location::File(PathBuf::from(location)),
    };
    Ok(Some(ArtifactValue {
        name: name.to_owned(),
        path: String::new(),
        location: Some(location),
        optional: false,
        directory: value.get("class").and_then(Json::as_str) == Some("Directory"),
    }))
}

/// The tags that a value of a lowered text gives: text as it is, the outputs of a loop as the
/// JSON list Argo aggregates them to, and an item with its fields.
fn tag_values(name: &str, value: &Json) -> Result<Vec<(String, String)>, String> {
    let text = |value: &Json| match value {
        Json::String(text) => text.clone(),
        other => Value::from_json(other).to_go_string(),
    };
    Ok(match value {
        Json::Null => Vec::new(),
        Json::Array(list) => {
            let results = name.ends_with(".outputs.result");
            let items = (list.iter().filter(|item| !item.is_null()))
                .map(|item| match results {
                    true => json_or_text(&text(item)),
                    false => Value::string(text(item)),
                })
                .collect();
            vec![(name.to_owned(), Value::Array(items).to_json())]
        }
        Json::Object(_) if name == "item" => {
            let (values, _) = item_variables("", 0, &Value::from_json(value))?;
            values.into_iter().collect()
        }
        other => vec![(name.to_owned(), text(other))],
    })
}

/// The items of a loop whose text is substituted, as `withParam` or `withSequence` gives them.
fn loop_items(
    text: &str,
    site: &str,
    assume_inputs: bool,
    warnings: &mut Vec<String>,
) -> Result<Vec<Value>, String> {
    let at = Position::default();
    let entry = |key: &str, value: Yaml| Entry {
        key: key.to_owned(),
        position: at,
        value: Node::new(value, at),
    };
    let mut fields = vec![entry("name", Yaml::String(site.to_owned()))];
    let sequence = match serde_json::from_str::<Json>(text) {
        Ok(Json::Object(object)) => {
            let bound = |key: &str| object.get(key).map(|value| AnyString(tag_text(value)));
            Some(Sequence {
                count: bound("count"),
                start: bound("start"),
                end: bound("end"),
                format: object.get("format").map(tag_text).unwrap_or_default(),
            })
        }
        _ => {
            fields.push(entry("withParam", Yaml::String(text.to_owned())));
            None
        }
    };
    let node = Node::new(Yaml::Mapping(fields), at);
    let mut warn = |warning| warnings.push(warning);
    items(&node, sequence.as_ref(), assume_inputs, &mut warn)
}

/// A bound of a sequence as Argo reads it: a text, or a number as its JSON text.
fn tag_text(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        other => other.to_string(),
    }
}
