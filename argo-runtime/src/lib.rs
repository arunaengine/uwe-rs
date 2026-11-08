//! Runs Argo Workflows documents as language-neutral tasks.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod adapter;
mod archives;
mod checks;
mod child;
mod claims;
mod conditions;
mod cursor;
mod dag;
mod environment;
pub mod expr;
mod git;
mod hooks;
mod http;
mod ignored;
mod inputs;
mod items;
pub mod jsonpath;
pub mod library;
mod marks;
mod merge;
mod node;
mod objects;
mod patch;
mod plugin;
mod pod;
mod references;
mod resources;
pub mod resume;
mod retry;
mod run;
mod schema;
mod scope;
pub mod sprig;
mod start;
mod steps;
mod sync;
pub mod tags;
mod time;
mod timers;
mod validate;
pub mod value;
mod volumes;
pub mod when;

use std::path::{Path, PathBuf};
use std::rc::Rc;

use argo_parser::{Node, Value as Yaml, parse_yaml};
use workflow_graph::{
    Delivery, DeliveryMode, Driver, DriverError, Files, Inputs, Language, Launch, Note, Outcome,
    Task,
};

pub use git::{Checkout, GitClient, GitSource};
pub use library::{Document, Kind, Library, load_file, repository_of};
pub use node::Facts;
pub use sync::SharedLocks;
pub use validate::{Rejection, validate};

/// Host-authorized paths; absent roots retain trusted local CLI behavior.
#[derive(Clone, Debug, Default)]
pub struct Policy {
    pub host_roots: Option<Vec<PathBuf>>,
    /// Explicit local backing directories, keyed by declared volume name.
    pub volumes: std::collections::BTreeMap<String, PathBuf>,
}

/// How a run behaves, besides its document and library.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// The template to start with instead of `spec.entrypoint`.
    pub entrypoint: Option<String>,
    /// Workflow parameters, in the order given; later ones win.
    pub parameters: Vec<(String, String)>,
    /// Where the run keeps files of its own.
    pub scratch: PathBuf,
    /// Print warnings to standard error as they come.
    pub print_warnings: bool,
    /// The creation time of the workflow; the time of the run when not set.
    pub now: Option<jiff::Timestamp>,
    /// Give workflow parameters without a value, and the inputs of the entrypoint that nothing
    /// supplies, placeholders, as a simulated run does (`Launch.simulated`). Without it, a run
    /// needs them, as `argo submit` does.
    pub assume_inputs: bool,
    /// Whether a DAG that does not set `failFast` goes on after a task failed, with the tasks
    /// that do not depend on it, as `failFast: false` does (`Launch.keep_going`).
    pub keep_going: bool,
    /// The S3 providers of the config file, for artifacts that name an endpoint.
    pub endpoints: Vec<Endpoint>,
    /// The folder where `uwe resume` leaves requests for the suspended steps of this run;
    /// without it, a real run cannot suspend without a duration.
    pub resume: Option<PathBuf>,
    /// The Git client that checks out git artifacts in a real run.
    pub git: Option<GitClient>,
    pub policy: Policy,
    pub locks: SharedLocks,
}

/// What a run of the command line gets from its host: the S3 providers of the config file,
/// the folder of resume requests and a Git client.
#[derive(Clone, Debug, Default)]
pub struct Services {
    pub endpoints: Vec<Endpoint>,
    pub resume: Option<PathBuf>,
    pub git: Option<GitClient>,
    pub policy: Policy,
    pub locks: SharedLocks,
}

/// An S3 storage provider: its name, the addresses it serves and its service, `None` for AWS.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Endpoint {
    pub provider: String,
    pub prefixes: Vec<String>,
    pub address: Option<String>,
}

/// A run of one Argo document as a [`Driver`].
pub struct ArgoDriver {
    document: Rc<Document>,
    library: Rc<Library>,
    options: Options,
    run: Option<run::Run>,
    warnings: Vec<String>,
    /// The folder that the output artifacts of the steps go to, as
    /// `<folder>/<step>/<artifact>`, in a real run; `None` keeps them in the store.
    pub results: Option<PathBuf>,
    deliveries: Vec<Delivery>,
    /// The files of the tasks, where a real executor keeps them.
    files: Option<std::sync::Arc<dyn Files>>,
    acceptance: std::collections::HashSet<(workflow_graph::TaskId, u32)>,
}

impl ArgoDriver {
    pub fn new(document: Rc<Document>, library: Rc<Library>, options: Options) -> Self {
        ArgoDriver {
            document,
            library,
            options,
            run: None,
            warnings: Vec::new(),
            results: None,
            deliveries: Vec::new(),
            files: None,
            acceptance: std::collections::HashSet::new(),
        }
    }

    /// What the run left out or replaced, like placeholder parameters or unmounted volumes.
    pub fn warnings(&self) -> &[String] {
        match &self.run {
            Some(run) => &run.warnings,
            None => &self.warnings,
        }
    }

    /// The name the run gave the workflow, once it started.
    pub fn workflow_name(&self) -> Option<&str> {
        self.run.as_ref().map(|run| run.workflow_name.as_str())
    }

    /// The outputs of a run that started, also of a simulated one: the output parameters and
    /// artifacts of the entrypoint, then the global outputs by their names, as
    /// [`Driver::outputs`] names them. A parameter is a text, an artifact a File or Directory.
    pub fn exported(&self) -> Option<serde_json::Value> {
        let run = self.run.as_ref()?;
        let mut values = serde_json::Map::new();
        if let Some(root) = run.root {
            let outputs = &run.nodes[root].outputs;
            for (name, value) in output_values(outputs) {
                values.entry(name).or_insert(value);
            }
        }
        for (key, value) in &run.globals {
            if let Some(name) = key.strip_prefix("workflow.outputs.parameters.") {
                values.entry(name).or_insert(value.as_str().into());
            }
        }
        for (key, artifact) in &run.global_artifacts {
            if let Some(name) = key.strip_prefix("workflow.outputs.artifacts.") {
                values.entry(name).or_insert(artifact_value(artifact));
            }
        }
        Some(serde_json::Value::Object(values))
    }
}

/// The output parameters of a node as texts and its output artifacts as Files or Directories,
/// by name, in the order of the node.
fn output_values(outputs: &node::NodeOutputs) -> Vec<(String, serde_json::Value)> {
    let parameters =
        (outputs.parameters.iter()).map(|(name, value)| (name.clone(), value.as_str().into()));
    let artifacts = (outputs.artifacts.iter())
        .map(|artifact| (artifact.name.clone(), artifact_value(artifact)));
    parameters.chain(artifacts).collect()
}

/// An output artifact as a File, or a Directory where it is one, named by the basename of its
/// path in the container. An artifact that the workflow holds as text has no file: null.
fn artifact_value(artifact: &node::ArtifactValue) -> serde_json::Value {
    let location = match &artifact.location {
        Some(node::Location::File(path)) => path.to_string_lossy().into_owned(),
        Some(node::Location::Url(url)) => url.clone(),
        Some(node::Location::Raw(_)) | None => return serde_json::Value::Null,
    };
    let class = match artifact.directory {
        true => "Directory",
        false => "File",
    };
    let path = artifact.path.trim_end_matches('/');
    let basename = path.rsplit('/').next().unwrap_or(path);
    let basename = match basename.is_empty() {
        true => artifact.name.as_str(),
        false => basename,
    };
    serde_json::json!({"class": class, "location": location, "basename": basename})
}

fn driver_error(failure: run::Failure) -> DriverError {
    if failure.unsupported {
        DriverError::unsupported(failure.message)
    } else {
        DriverError::new(failure.message)
    }
}

impl Driver for ArgoDriver {
    fn language(&self) -> Language {
        Language::Argo
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        let run = run::Run::new(
            Rc::clone(&self.document),
            Rc::clone(&self.library),
            self.options.clone(),
        )
        .map_err(driver_error)?;
        let run = self.run.insert(run);
        run.files = self.files.clone();
        run.operate().map_err(driver_error)
    }

    fn use_files(&mut self, files: std::sync::Arc<dyn Files>) {
        self.files = Some(files);
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        let identity = (outcome.id, outcome.attempt);
        let Some(run) = &mut self.run else {
            return Err(DriverError::new("the run has not started"));
        };
        let completed = run.complete(outcome);
        if run.take_acceptance(identity.0, identity.1) {
            self.acceptance.insert(identity);
        }
        completed.map_err(driver_error)
    }

    fn take_acceptance(&mut self, task: workflow_graph::TaskId, attempt: u32) -> bool {
        self.acceptance.remove(&(task, attempt))
    }

    fn wake_at(&self) -> Option<std::time::Instant> {
        let run = self.run.as_ref()?;
        let next = run.wake_at()?;
        Some(if run.options.assume_inputs && !run.active_tasks() {
            std::time::Instant::now()
        } else {
            next
        })
    }

    fn wake(&mut self) -> Result<Vec<Task>, DriverError> {
        let run = self
            .run
            .as_mut()
            .ok_or_else(|| DriverError::new("the run has not started"))?;
        let now = if run.options.assume_inputs {
            run.wake_at().unwrap_or(run.clock)
        } else {
            std::time::Instant::now()
        };
        run.wake(now).map_err(driver_error)
    }

    fn handles_error(
        &self,
        task: workflow_graph::TaskId,
        error: &workflow_graph::ExecuteError,
    ) -> bool {
        matches!(
            error,
            workflow_graph::ExecuteError::Machine { .. }
                | workflow_graph::ExecuteError::Executor { .. }
        ) || (matches!(error, workflow_graph::ExecuteError::Cancelled)
            && self.run.as_ref().is_some_and(|run| run.expired_task(task)))
    }

    fn cancellations(&mut self) -> Vec<workflow_graph::TaskId> {
        self.run
            .as_mut()
            .map(run::Run::cancellations)
            .unwrap_or_default()
    }

    /// Ends the run, and delivers the output artifacts of its steps to the results folder.
    fn finish(&mut self) -> Result<(), DriverError> {
        let Some(run) = &self.run else {
            return Err(DriverError::new("the run has not started"));
        };
        let result = run.result().map_err(DriverError::new);
        // Requests that came after the last suspended step are of no use any more.
        if let Some(folder) = &run.options.resume {
            let _ = std::fs::remove_dir_all(folder);
        }
        if let Some(folder) = &self.results {
            for (step, artifact, location) in run.output_artifacts() {
                self.deliveries.push(Delivery {
                    location: location.to_string_lossy().into_owned(),
                    target: folder
                        .join(step)
                        .join(artifact)
                        .to_string_lossy()
                        .into_owned(),
                    mode: DeliveryMode::Copy,
                    overwrite: true,
                });
            }
        }
        result
    }

    fn deliveries(&mut self) -> Vec<Delivery> {
        let mut deliveries = std::mem::take(&mut self.deliveries);
        if let Some(run) = &mut self.run {
            deliveries.extend(run.take_deliveries());
        }
        deliveries
    }

    fn notes(&mut self) -> Vec<Note> {
        (self.run.as_mut())
            .map(|run| std::mem::take(&mut run.notes))
            .unwrap_or_default()
    }

    /// The parameters and input artifacts of the workflow, with the values of the run.
    fn inputs(&mut self) -> Result<Option<Inputs>, DriverError> {
        schema::inputs(&self.document, &self.library, &self.options)
            .map(Some)
            .map_err(driver_error)
    }

    /// The outputs of the entrypoint and the global outputs of the workflow.
    fn outputs(&mut self) -> Result<Option<serde_json::Value>, DriverError> {
        schema::outputs(&self.document, &self.library, &self.options)
            .map(Some)
            .map_err(driver_error)
    }

    fn results(&self) -> Option<serde_json::Value> {
        self.exported().filter(|_| !self.options.assume_inputs)
    }
}

/// The driver for an Argo workflow file.
pub fn driver(launch: &Launch) -> Result<Box<dyn Driver>, DriverError> {
    driver_with(launch, Services::default())
}

/// The driver for an Argo workflow file, with the services of its host.
pub fn driver_with(launch: &Launch, services: Services) -> Result<Box<dyn Driver>, DriverError> {
    Ok(Box::new(load_driver(launch, services)?))
}

/// The driver for an Argo workflow file with the services of its host, once it is loaded.
pub fn load_driver(launch: &Launch, services: Services) -> Result<ArgoDriver, DriverError> {
    let (document, library, options) = load_launch(launch, services)?;
    let mut driver = ArgoDriver::new(document, Rc::new(library), options);
    if !launch.simulated {
        let folder = launch
            .output_dir
            .clone()
            .unwrap_or_else(|| "results".into());
        // An address of a storage provider, like `s3://bucket/results`, stays as it is.
        let address = folder.to_string_lossy().contains("://");
        driver.results = Some(match address {
            true => folder,
            false => std::path::absolute(&folder).unwrap_or(folder),
        });
    }
    Ok(driver)
}

/// The document that a launch runs, its library of templates and the options of its run.
fn load_launch(
    launch: &Launch,
    services: Services,
) -> Result<(Rc<Document>, Library, Options), DriverError> {
    let target = &launch.target;
    if target.is_dir() {
        return Err(DriverError::new(format!(
            "{} is a directory; give the Argo workflow file",
            target.display()
        )));
    }
    let documents = load_file(target).map_err(DriverError::new)?;
    let document = documents
        .iter()
        .find(|document| document.kind == Kind::Workflow && document.runnable())
        .or_else(|| documents.iter().find(|document| document.runnable()))
        .cloned()
        .ok_or_else(|| {
            DriverError::new(format!(
                "{} holds no Argo document with an entrypoint",
                target.display()
            ))
        })?;
    let library = Library::new();
    library.add_documents(0, documents);
    for config in &launch.configs {
        if config.is_dir() {
            library.add_directory(1, config);
        } else {
            library.add_documents(1, load_file(config).map_err(DriverError::new)?);
        }
    }
    library.add_directory(2, repository_of(target));
    let mut parameters = Vec::new();
    if let Some(file) = &launch.inputs {
        parameters.extend(parameter_file(file).map_err(DriverError::new)?);
    }
    parameters.extend(launch.params.iter().cloned());
    let options = Options {
        entrypoint: launch.targets.first().cloned(),
        parameters,
        scratch: launch.scratch.clone(),
        print_warnings: true,
        now: None,
        assume_inputs: launch.simulated,
        keep_going: launch.keep_going,
        endpoints: services.endpoints,
        resume: services.resume,
        git: services.git,
        policy: services.policy,
        locks: services.locks,
    };
    Ok((document, library, options))
}

/// The parameters of a parameter file, a YAML or JSON mapping, as `argo submit
/// --parameter-file` reads it: texts as they are, other values as JSON.
pub fn parameter_file(path: &Path) -> Result<Vec<(String, String)>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let node = parse_yaml(&text)
        .map_err(|error| format!("{}:{}: {}", path.display(), error.position, error.message))?;
    let Some(Node {
        value: Yaml::Mapping(entries),
        ..
    }) = node
    else {
        return Err(format!(
            "{} must hold a mapping of parameter names to values",
            path.display()
        ));
    };
    Ok(entries
        .into_iter()
        .map(|entry| {
            let value = match &entry.value.value {
                Yaml::String(text) => text.clone(),
                _ => pod::node_json(&entry.value),
            };
            (entry.key, value)
        })
        .collect())
}
