//! Runs a Nextflow script: the runtime asks for tasks, the task runner executes or reuses them.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use content_store::Store;
use executor::local::LocalContainer;
use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::value::PathValue;
use nextflow_runtime::task::{
    ENVIRONMENT, STDOUT, command_script, container, read_environment, task_request,
};
use nextflow_runtime::{Loader, Options, Run, TaskOutcome, TaskRequest};
use task_runner::Runner;

pub struct Settings {
    pub store: PathBuf,
    pub scratch: PathBuf,
    pub engine: String,
    pub stub: bool,
}

/// Why a run failed, and whether the cause is a feature iunctus does not support yet.
pub struct Failure {
    pub message: String,
    pub unsupported: bool,
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Failure {
            unsupported: matches!(error, Error::Unsupported { .. }),
            message: error.to_string(),
        }
    }
}

fn failure(message: impl Into<String>) -> Failure {
    Failure {
        message: message.into(),
        unsupported: false,
    }
}

/// Prints what the workflow prints.
struct Console;

impl Host for Console {
    fn print(&mut self, text: &str) {
        print!("{text}");
    }
}

pub fn run(script: &Path, settings: &Settings) -> Result<(), Failure> {
    let module = Loader::default().load(script).map_err(|error| Failure {
        unsupported: error
            .messages
            .iter()
            .any(|message| message.contains("not supported yet")),
        message: error.to_string(),
    })?;
    let store = Store::local(&settings.store).map_err(|error| failure(error.to_string()))?;
    let executor = LocalContainer {
        engine: settings.engine.clone(),
        scratch: settings.scratch.clone(),
    };
    let run_id = run_id();
    let runner = Runner::new(
        store,
        executor,
        &run_id,
        &settings.scratch.join("downloads"),
    )
    .map_err(|error| failure(error.to_string()))?;
    let options = Options {
        stub: settings.stub,
        container_engine: Some(settings.engine.clone()),
    };
    let mut console = Console;
    let (mut run, mut queue) = Run::start(&module, &options, &mut console)?;
    let mut pinned = HashMap::new();
    while !queue.is_empty() {
        let request = queue.remove(0);
        let outcome = execute(&runner, &request, &mut pinned)?;
        queue.extend(run.complete(outcome, &mut console)?);
    }
    run.finish()?;
    Ok(())
}

/// Executes or reuses one task and turns its stored result into the outcome the runtime reads.
fn execute(
    runner: &Runner<LocalContainer>,
    request: &TaskRequest,
    pinned: &mut HashMap<String, Option<String>>,
) -> Result<TaskOutcome, Failure> {
    let problem = |error: &dyn std::fmt::Display| failure(format!("{}: {error}", request.name));
    let image = match container(request) {
        Some(image) => {
            if !pinned.contains_key(&image) {
                let digest = runner
                    .executor()
                    .pin(&image)
                    .map_err(|error| problem(&error))?;
                pinned.insert(image.clone(), digest);
            }
            Some(pinned[&image].clone().unwrap_or(image))
        }
        None => None,
    };
    let script = runner
        .import_bytes(command_script(request).as_bytes())
        .map_err(|error| problem(&error))?;
    let mut inputs = Vec::new();
    for (source, _) in &request.staged {
        if source.prefix.is_some() {
            return Err(Failure {
                message: format!(
                    "{}: remote input files are not supported yet: {}",
                    request.name,
                    source.uri()
                ),
                unsupported: true,
            });
        }
        let (digest, _) = runner
            .import(Path::new(&source.path))
            .map_err(|error| problem(&error))?;
        inputs.push(digest);
    }
    let model = task_request(request, image, script, &inputs);
    let result = runner.execute(&model).map_err(|error| problem(&error))?;
    println!(
        "[{}] {}",
        if result.reused { "cached" } else { "done" },
        request.name
    );
    let mut outcome = TaskOutcome {
        id: request.id,
        exit_status: result.exit_code.unwrap_or(-1),
        ..TaskOutcome::default()
    };
    for file in &result.files {
        let text = |runner: &Runner<LocalContainer>| {
            runner
                .read(&file.location)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        };
        match file.path.as_str() {
            STDOUT => outcome.stdout = text(runner).map_err(|error| problem(&error))?,
            ENVIRONMENT => {
                let variables = read_environment(&text(runner).map_err(|error| problem(&error))?);
                for (index, _) in request.eval.iter().enumerate() {
                    let (value, status) = variables
                        .get(&format!("nxf_out_eval_{}", index + 1))
                        .cloned()
                        .unwrap_or_default();
                    if status != 0 {
                        return Err(failure(format!(
                            "{}: an `eval` output command failed with exit status {status}: {value}",
                            request.name
                        )));
                    }
                    outcome.eval.push(value);
                }
                for name in &request.env {
                    let value = variables
                        .get(&**name)
                        .map(|(value, _)| value.clone())
                        .unwrap_or_default();
                    outcome.env.insert(name.to_string(), value);
                }
            }
            _ => {}
        }
        let location = runner
            .local_path(&file.location)
            .unwrap_or_else(|| PathBuf::from(&file.location));
        let path = PathValue::parse(&location.to_string_lossy(), "/");
        outcome.files.push((file.path.clone(), path));
    }
    Ok(outcome)
}

/// A run id from the clock and the process id.
fn run_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    format!("{now:x}-{:x}", std::process::id())
}
