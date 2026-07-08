//! Runs Argo Workflows documents as language-neutral tasks, following the controller of Argo
//! v4.1.4: template resolution through `templateRef` and `workflowTemplateRef`, Argo's
//! substitution phases, expr-lang and govaluate expressions, steps, DAGs, loops, retries, exit
//! handlers and hooks. Each leaf template becomes a [`workflow_graph::Task`].
//!
//! [`driver`] is the entry point for a [`Launch`]: the workflow file, a parameter file
//! (`Launch.inputs`, like `argo submit --parameter-file`), parameters (`Launch.params`, like
//! `-p`), an entrypoint (the first of `Launch.targets`), and extra files or directories of
//! templates (`Launch.configs`). Templates are also looked up in the repository of the workflow
//! file: the nearest directory above it that holds `.git`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod checks;
mod child;
mod claims;
mod conditions;
mod dag;
mod environment;
pub mod expr;
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
mod plugin;
mod pod;
mod resources;
mod retry;
mod run;
mod scope;
pub mod sprig;
mod start;
mod steps;
pub mod tags;
mod time;
mod validate;
pub mod value;
mod volumes;
pub mod when;

use std::path::{Path, PathBuf};
use std::rc::Rc;

use argo_parser::{Node, Value as Yaml, parse_yaml};
use workflow_graph::{
    Delivery, DeliveryMode, Driver, DriverError, Language, Launch, Note, Outcome, Task,
};

pub use library::{Document, Kind, Library, load_file, repository_of};
pub use validate::{Rejection, validate};

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
        run.operate().map_err(driver_error)
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        let Some(run) = &mut self.run else {
            return Err(DriverError::new("the run has not started"));
        };
        run.complete(outcome).map_err(driver_error)
    }

    /// Ends the run, and delivers the output artifacts of its steps to the results folder.
    fn finish(&mut self) -> Result<(), DriverError> {
        let Some(run) = &self.run else {
            return Err(DriverError::new("the run has not started"));
        };
        let result = run.result().map_err(DriverError::new);
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
                });
            }
        }
        result
    }

    fn deliveries(&mut self) -> Vec<Delivery> {
        std::mem::take(&mut self.deliveries)
    }

    fn notes(&mut self) -> Vec<Note> {
        (self.run.as_mut())
            .map(|run| std::mem::take(&mut run.notes))
            .unwrap_or_default()
    }
}

/// The driver for an Argo workflow file.
pub fn driver(launch: &Launch) -> Result<Box<dyn Driver>, DriverError> {
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
    };
    let mut driver = ArgoDriver::new(document, Rc::new(library), options);
    if !launch.simulated {
        let folder = launch
            .output_dir
            .clone()
            .unwrap_or_else(|| "results".into());
        driver.results = Some(std::path::absolute(&folder).unwrap_or(folder));
    }
    Ok(Box::new(driver))
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
