//! The WDL driver of the execution graph: it loads a document and its inputs, runs the main
//! workflow or task, and delivers its output files.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::Value as Json;
use wdl_ast::AstToken;
use wdl_ast::v1::{CallStatement, WorkflowStatement};
use workflow_graph::{
    Delivery, DeliveryMode, Driver, DriverError, Files, Inputs, Language, Launch, LocalFiles,
    Outcome, Task, TaskId,
};

use crate::document::{self, Callable, Unit};
use crate::schema;
use crate::task::{DEFAULT_IMAGE, Settings};
use crate::value::Value;
use crate::workflow::Run;

/// What a run of a WDL document may change.
#[derive(Clone, Debug)]
pub struct Options {
    /// The image of tasks without a container.
    pub image: String,
    /// Whether a real run prints its outputs.
    pub print: bool,
    /// Reads the documents at web addresses that the document imports, instead of the analysis.
    pub imports: Option<document::Imports>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            image: DEFAULT_IMAGE.into(),
            print: true,
            imports: None,
        }
    }
}

/// Runs one WDL document with one set of inputs.
pub struct WdlDriver {
    launch: Launch,
    unit: Unit,
    run: Run,
    print: bool,
    deliveries: Vec<Delivery>,
    results: Option<Json>,
    /// The outputs of the finished run, at the paths they were delivered to.
    outputs: Option<IndexMap<String, Value>>,
    acceptance: HashSet<(TaskId, u32)>,
}

/// The driver of a WDL launch with the default options.
pub fn driver(launch: &Launch) -> Result<Box<dyn Driver>, DriverError> {
    driver_with(launch, Options::default())
}

pub fn driver_with(launch: &Launch, options: Options) -> Result<Box<dyn Driver>, DriverError> {
    Ok(Box::new(WdlDriver::new(launch, options)?))
}

impl WdlDriver {
    /// Loads the document of a launch; `Launch.targets` may name the workflow or task to run.
    pub fn new(launch: &Launch, options: Options) -> Result<Self, DriverError> {
        let document = document::load(&launch.target, options.imports.as_ref())?;
        let unit = document::entry(&document, launch.targets.first().map(String::as_str))?;
        let settings = Settings {
            image: options.image,
            simulated: launch.simulated,
            files: Arc::new(LocalFiles),
        };
        let base = match launch.scratch.as_os_str().is_empty() {
            true => std::env::temp_dir().join("uwe-wdl"),
            false => launch.scratch.join("wdl"),
        };
        // Blocks and runs can share the scratch folder, so each driver writes in its own.
        let scratch = std::fs::create_dir_all(&base)
            .and_then(|()| tempfile::Builder::new().prefix("driver-").tempdir_in(&base))
            .map_err(|error| DriverError::new(format!("cannot make a scratch folder: {error}")))?;
        Ok(WdlDriver {
            launch: launch.clone(),
            unit,
            run: Run::new(settings, scratch, launch.keep_going),
            print: options.print,
            deliveries: Vec::new(),
            results: None,
            outputs: None,
            acceptance: HashSet::new(),
        })
    }

    /// The outputs of a finished run, also of a simulated one, by their names without the
    /// name of the workflow, as [`Value::result_json`] gives them.
    pub fn exported(&self) -> Option<Json> {
        let outputs = self.outputs.as_ref()?;
        let object = (outputs.iter()).map(|(name, value)| (name.clone(), value.result_json()));
        Some(Json::Object(object.collect()))
    }

    /// The inputs of the main workflow or task, and of its calls, from the launch.
    fn given(&self) -> Result<schema::Given, DriverError> {
        let calls = |alias: &str| {
            let Callable::Workflow(workflow) = &self.unit.callable else {
                return None;
            };
            let call = find_call(workflow.statements(), alias)?;
            let unit = document::called(&self.unit.document, &call.target()).ok()?;
            Some(schema::input_types(&unit))
        };
        schema::given(
            &self.unit,
            self.launch.inputs.as_deref(),
            &self.launch.params,
            &calls,
        )
        .map_err(DriverError::new)
    }

    /// Delivers the files of the outputs to `folder/<output>/`, numbered for arrays, and
    /// gives the outputs with the delivered paths.
    fn deliver(
        &mut self,
        outputs: &IndexMap<String, Value>,
        folder: &str,
    ) -> IndexMap<String, Value> {
        let mut delivered = IndexMap::new();
        for (name, value) in outputs {
            let mut value = value.clone();
            let mut count = 0;
            value.visit_paths(&mut |_, _| count += 1);
            let mut index = 0;
            value.visit_paths(&mut |path, _| {
                let base = path
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or("output")
                    .to_owned();
                let target = match count {
                    1 => format!("{folder}/{name}/{base}"),
                    _ => format!("{folder}/{name}/{index}/{base}"),
                };
                index += 1;
                // A copy onto its own source would remove it first.
                if !same_file(path, &target) {
                    self.deliveries.push(Delivery {
                        location: path.clone(),
                        target: target.clone(),
                        mode: DeliveryMode::Copy,
                        overwrite: true,
                    });
                }
                *path = target;
            });
            delivered.insert(name.clone(), value);
        }
        delivered
    }
}

/// Whether two local paths name one file, also through links.
fn same_file(source: &str, target: &str) -> bool {
    let resolved = |path: &str| std::fs::canonicalize(path).or_else(|_| std::path::absolute(path));
    matches!((resolved(source), resolved(target)), (Ok(source), Ok(target)) if source == target)
}

/// The call of a workflow with a name, also inside scatters and conditionals.
fn find_call(
    statements: impl Iterator<Item = WorkflowStatement>,
    alias: &str,
) -> Option<CallStatement> {
    for statement in statements {
        let found = match statement {
            WorkflowStatement::Call(call) => {
                let name = match call.alias() {
                    Some(alias) => alias.name().text().to_owned(),
                    None => call
                        .target()
                        .names()
                        .last()
                        .map(|name| name.text().to_owned())
                        .unwrap_or_default(),
                };
                (name == alias).then_some(call)
            }
            WorkflowStatement::Scatter(scatter) => find_call(scatter.statements(), alias),
            WorkflowStatement::Conditional(conditional) => {
                (conditional.clauses()).find_map(|clause| find_call(clause.statements(), alias))
            }
            WorkflowStatement::Declaration(_) => None,
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

impl Driver for WdlDriver {
    fn language(&self) -> Language {
        Language::Wdl
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        let given = self.given()?;
        self.run.start(self.unit.clone(), given.values, given.calls)
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        let failures = self.run.failures.len();
        let tasks = self.run.complete(&outcome)?;
        if self.run.failures.len() == failures {
            self.acceptance.insert((outcome.id, outcome.attempt));
        }
        Ok(tasks)
    }

    fn take_acceptance(&mut self, task: TaskId, attempt: u32) -> bool {
        self.acceptance.remove(&(task, attempt))
    }

    fn draining(&self) -> bool {
        !self.run.failures.is_empty() && !self.launch.keep_going
    }

    fn use_files(&mut self, files: Arc<dyn Files>) {
        self.run.use_files(files);
    }

    fn inputs(&mut self) -> Result<Option<Inputs>, DriverError> {
        let given = self.given()?.values;
        let values: serde_json::Map<String, Json> = (given.iter())
            .filter(|(_, value)| **value != Value::None)
            .map(|(name, value)| (name.clone(), value.to_json()))
            .collect();
        Ok(Some(Inputs {
            schema: schema::inputs(&self.unit),
            values: Json::Object(values),
            base: std::env::current_dir().unwrap_or_default(),
            read: None,
            assumes_values: false,
            advisory: false,
            ignored: Default::default(),
        }))
    }

    fn outputs(&mut self) -> Result<Option<Json>, DriverError> {
        Ok(Some(schema::outputs(&self.unit)))
    }

    fn results(&self) -> Option<Json> {
        self.results.clone()
    }

    fn deliveries(&mut self) -> Vec<Delivery> {
        std::mem::take(&mut self.deliveries)
    }

    /// A real run delivers the files of the outputs and prints them, keyed like the outputs
    /// of other WDL engines, as `workflow.output`.
    fn finish(&mut self) -> Result<(), DriverError> {
        if let Some(first) = self.run.failures.first() {
            let message = match self.run.failures.len() {
                1 => first.clone(),
                count => format!("{first}; and {} more calls failed", count - 1),
            };
            return Err(DriverError::new(message));
        }
        let Some(outputs) = self.run.outputs.clone() else {
            let waiting = self.run.waiting();
            return Err(DriverError::new(match waiting.is_empty() {
                true => "the run ended before the workflow finished".to_owned(),
                false => format!(
                    "these steps wait for values that never came: {}",
                    waiting.join("; ")
                ),
            }));
        };
        let outputs = match self.launch.simulated {
            true => outputs,
            false => {
                let folder = output_folder(self.launch.output_dir.as_deref());
                self.deliver(&outputs, &folder)
            }
        };
        let name = self.unit.name();
        let object: serde_json::Map<String, Json> = (outputs.iter())
            .map(|(output, value)| (format!("{name}.{output}"), value.to_json()))
            .collect();
        let object = Json::Object(object);
        if self.print && !self.launch.simulated {
            println!(
                "{}",
                serde_json::to_string_pretty(&object).unwrap_or_default()
            );
        }
        self.results = Some(object);
        self.outputs = Some(outputs);
        Ok(())
    }
}

/// Where output files go: the `-output-dir` folder or address, else the current folder.
fn output_folder(folder: Option<&Path>) -> String {
    let folder: PathBuf = match folder {
        Some(folder) if folder.to_string_lossy().contains("://") => {
            return folder.to_string_lossy().trim_end_matches('/').to_owned();
        }
        Some(folder) => std::path::absolute(folder).unwrap_or(folder.to_owned()),
        None => std::env::current_dir().unwrap_or_default(),
    };
    folder.to_string_lossy().trim_end_matches('/').to_owned()
}
