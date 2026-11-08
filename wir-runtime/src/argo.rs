//! Argo in wir: native blocks run the Argo driver of uwe as a child, dialect tasks run single
//! steps and DAG tasks through the Argo adapter, and argo expr ops substitute Argo texts.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use argo_runtime::adapter::{Adapter, CallId as ArgoCall, Form, Instance};
use serde_json::{Map, Value as Json};
use wir::{Binding, ExprForm, Port, PortMap, TaskBuilder, TaskRef, TypeDef};
use workflow_graph::{Delivery, DriverError, ExecuteError, Launch, Outcome, Task, TaskId};

use crate::adapter::{Adapters, CallId, Changes, Ending, Opening, TaskAdapter, TaskCall};
use crate::child::Block;

/// An Argo driver for the launch of one block, with the services of the run.
pub(crate) fn block(
    launch: &Launch,
    services: &argo_runtime::Services,
) -> Result<Box<dyn Block>, DriverError> {
    let driver = argo_runtime::load_driver(launch, services.clone())?;
    Ok(Box::new(driver))
}

impl Block for argo_runtime::ArgoDriver {
    /// The outputs of the entrypoint and the global outputs, which a simulated run gives too.
    fn exported(&self) -> Result<Option<Json>, DriverError> {
        Ok(argo_runtime::ArgoDriver::exported(self))
    }
}

/// The adapters with the one that runs `argo_task` dialect tasks, with the services of the run.
pub fn register(mut adapters: Adapters, services: argo_runtime::Services) -> Adapters {
    adapters.register("argo_task", move |opening: &Opening| {
        let place = Place {
            source: &opening.source,
            configs: &opening.configs,
            folder: &opening.folder,
            output: &opening.output,
        };
        let mut adapter = load(&place, opening.inputs, opening.launch, &services)?;
        if let Some(files) = &opening.files {
            adapter.use_files(files.clone());
        }
        let results = (!opening.launch.simulated).then(|| opening.output.clone());
        let tasks = ArgoTasks {
            adapter,
            calls: BTreeMap::new(),
            results,
        };
        Ok(Box::new(tasks) as Box<dyn TaskAdapter>)
    });
    adapters
}

/// Where an Argo adapter runs: its document, configs, own folder and output folder.
pub(crate) struct Place<'a> {
    pub source: &'a Path,
    pub configs: &'a [PathBuf],
    pub folder: &'a Path,
    pub output: &'a Path,
}

/// The adapter of an Argo document, with the inputs of the run as workflow parameters.
pub(crate) fn load(
    place: &Place,
    inputs: &Map<String, Json>,
    run: &Launch,
    services: &argo_runtime::Services,
) -> Result<Adapter, DriverError> {
    let job = place.folder.join("parameters.json");
    let written = std::fs::create_dir_all(place.folder)
        .and_then(|()| std::fs::write(&job, crate::nextflow::params(inputs).to_string()));
    written
        .map_err(|error| DriverError::new(format!("cannot write {}: {error}", job.display())))?;
    let launch = Launch {
        target: place.source.to_owned(),
        inputs: Some(job),
        configs: place.configs.to_vec(),
        scratch: place.folder.to_owned(),
        output_dir: Some(place.output.to_owned()),
        stub: run.stub,
        simulated: run.simulated,
        keep_going: run.keep_going,
        ..Launch::default()
    };
    Adapter::load(&launch, services.clone())
}

/// The configs of an Argo source: those of its task or native ops, which an argo expr op
/// substitutes its text with too.
pub(crate) fn configs_of(document: &wir::Document, source: &str) -> Vec<String> {
    let ops = document.graphs.iter().flat_map(|graph| &graph.ops);
    let configs = ops.filter_map(|op| match &op.kind {
        wir::Kind::Task(wir::Task {
            builder: Some(TaskBuilder::ArgoTask(task)),
            ..
        }) if task.source == source => Some(&task.configs),
        wir::Kind::Native(native) if native.source == source => Some(&native.configs),
        _ => None,
    });
    configs.into_iter().next().cloned().unwrap_or_default()
}

/// The value of an argo expr op, substituted by the adapter of its source.
pub(crate) fn evaluate(
    adapter: &mut Adapter,
    spec: &wir::Expr,
    bound: &Map<String, Json>,
    output: &Port,
    defs: &[TypeDef],
) -> Result<Json, String> {
    let form = match spec.form.unwrap_or(ExprForm::Text) {
        ExprForm::Text => Form::Text,
        ExprForm::Condition => Form::Condition,
        ExprForm::Items => Form::Items,
        ExprForm::Labels => Form::Labels,
    };
    let value = adapter.evaluate(&spec.code, form, "", bound)?;
    match wir::fits(&value, &output.r#type, defs) {
        true => Ok(value),
        false => Err(format!(
            "the value {value} does not fit output {}",
            output.name
        )),
    }
}

/// The adapters that argo expr ops evaluate with, by source.
#[derive(Default)]
pub(crate) struct Texts(pub BTreeMap<String, Adapter>);

/// The dialect tasks of one Argo document: each op instance is a call of the adapter.
struct ArgoTasks {
    adapter: Adapter,
    /// The calls that have not ended, with the op instance and the outputs of their task.
    calls: BTreeMap<ArgoCall, (CallId, Vec<PortMap>)>,
    /// The results folder that the output artifacts go to once the run ends, as natively.
    results: Option<PathBuf>,
}

impl ArgoTasks {
    /// The tasks with the calls that asked for them, and the calls that ended among those
    /// that `touched` names, or among all.
    fn changes(
        &mut self,
        tasks: Vec<Task>,
        touched: Option<Vec<ArgoCall>>,
    ) -> Result<Changes, DriverError> {
        let mut changes = Changes::default();
        let mut touched = touched;
        for task in tasks {
            let native = self.adapter.call_of(task.id);
            let owner = native.and_then(|native| self.calls.get(&native));
            let (Some(native), Some((owner, _))) = (native, owner) else {
                let message = format!("task {} belongs to no open Argo call", task.name);
                return Err(DriverError::new(message));
            };
            changes.tasks.push((*owner, task));
            if let Some(touched) = &mut touched {
                touched.push(native);
            }
        }
        let touched = touched.unwrap_or_else(|| self.calls.keys().copied().collect());
        for native in touched {
            let Some(ending) = self.adapter.ending(native) else {
                continue;
            };
            if let Some((call, outputs)) = self.calls.remove(&native) {
                changes.ended.push((call, ended(&ending, &outputs)));
            }
        }
        Ok(changes)
    }
}

impl TaskAdapter for ArgoTasks {
    fn start(&mut self, call: &TaskCall) -> Result<Changes, DriverError> {
        let TaskBuilder::ArgoTask(task) = call.builder else {
            return Err(DriverError::new("an Argo adapter runs only argo_task ops"));
        };
        let (native, tasks) = self.adapter.start(&instance(task, call))?;
        (self.calls).insert(native, (call.call, task.outputs.clone()));
        self.changes(tasks, Some(vec![native]))
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Changes, DriverError> {
        let native = self.adapter.call_of(outcome.id);
        let tasks = self.adapter.complete(outcome)?;
        self.changes(tasks, native.map(|native| vec![native]))
    }

    fn take_acceptance(&mut self, task: TaskId, attempt: u32) -> bool {
        self.adapter.take_acceptance(task, attempt)
    }

    fn handles_error(&self, task: TaskId, error: &ExecuteError) -> bool {
        self.adapter.handles_error(task, error)
    }

    /// A call that waits, like for the backoff of a retry; the deadline of the workflow alone
    /// keeps no run waiting once its calls ended.
    fn wake_at(&self) -> Option<Instant> {
        self.adapter.wake_at().filter(|_| !self.calls.is_empty())
    }

    fn wake(&mut self) -> Result<Changes, DriverError> {
        let tasks = self.adapter.wake(Instant::now())?;
        self.changes(tasks, None)
    }

    fn cancellations(&mut self) -> Vec<TaskId> {
        self.adapter.cancellations()
    }

    fn deliveries(&mut self) -> Vec<Delivery> {
        self.adapter.take_deliveries()
    }

    fn end(&mut self, _failure: Option<&str>) -> Result<Vec<Delivery>, DriverError> {
        if let Some(results) = &self.results {
            self.adapter.finish(results);
        }
        Ok(self.adapter.take_deliveries())
    }

    /// Argo names its tasks by their node names, as the native run does.
    fn prefixed(&self) -> bool {
        false
    }
}

/// The call of a dialect task: its holding template and step, its node path, the round of its
/// loop, and the values of the tags it reads.
fn instance(task: &TaskRef, call: &TaskCall) -> Instance {
    let reference = match task.captures.get("template_ref") {
        Some(Binding::Value(value)) => (value.get("name").and_then(Json::as_str)).map(|name| {
            let cluster = value.get("cluster_scope").and_then(Json::as_bool);
            (name.to_owned(), cluster.unwrap_or(false))
        }),
        _ => None,
    };
    let mut path = Vec::new();
    let mut values = Vec::new();
    for map in &task.inputs {
        let value = call.inputs.get(&map.port).cloned().unwrap_or(Json::Null);
        if tag(map) == "node.path" {
            let parts = value.as_array().into_iter().flatten();
            path = parts
                .filter_map(|part| part.as_str().map(str::to_owned))
                .collect();
            continue;
        }
        values.push((tag(map).to_owned(), value));
    }
    Instance {
        template: task.entry.clone(),
        reference,
        call: task.call.clone(),
        path,
        index: call.frame.rounds.last().map_or(0, |round| *round as usize),
        values,
    }
}

/// The tag of a native selector like `parameter/outputs.result`.
fn tag(map: &PortMap) -> &str {
    (map.native.split_once('/')).map_or(map.native.as_str(), |(_, name)| name)
}

/// How a call ended in wir: its outputs by port when it succeeded, and those that Argo gives a
/// skipped step, like the defaults of its output parameters.
fn ended(ending: &argo_runtime::adapter::Ending, outputs: &[PortMap]) -> Ending {
    let facts = &ending.facts;
    let given = |map: &PortMap| {
        ending
            .outputs
            .get(tag(map))
            .map(|v| (map.port.clone(), v.clone()))
    };
    // An output that Argo gives no value is null, so that a step reading it still runs and
    // takes its own default, as the native run does.
    let values =
        (outputs.iter()).map(|map| given(map).unwrap_or_else(|| (map.port.clone(), Json::Null)));
    if facts.succeeded {
        return Ending::Succeeded {
            values: values.collect(),
            empty: Vec::new(),
        };
    }
    match (facts.skipped || facts.omitted, facts.errored) {
        (true, _) => Ending::Skipped(values.collect()),
        (false, true) => Ending::Errored(ending.message.clone()),
        (false, false) => Ending::Failed(ending.message.clone()),
    }
}
