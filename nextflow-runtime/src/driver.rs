//! The Nextflow driver: a run of a script behind the language-neutral driver interface of
//! `workflow-graph`. It loads the config and the main script, starts the dataflow network,
//! turns each task the network asks for into a container task, and each outcome back.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use nextflow_config::{Config, tree};
use nextflow_evaluator::interpreter::{Error, Host, parse_json, parse_yaml};
use nextflow_evaluator::value::{PathValue, Value};
use workflow_graph::{Driver, DriverError, Input, Language, Launch, Outcome, Task};

use crate::process::glob_matches;
use crate::task::{
    ENVIRONMENT, SCRIPT, command, command_script, container, read_environment, resources,
};
use crate::{LoadError, Loader, Options, Run, TaskId, TaskOutcome, TaskRequest};

/// What a Nextflow run takes besides the [`Launch`].
pub struct Settings {
    /// Prints what the workflow prints and reads its files.
    pub host: Box<dyn Host>,
    /// The container engine, as `workflow.containerEngine` shows it.
    pub container_engine: Option<String>,
    /// The name of the run, `workflow.runName`.
    pub run_name: String,
    /// The command line that started the run, `workflow.commandLine`.
    pub command_line: String,
}

impl Default for Settings {
    /// Prints to standard output and reads local files only.
    fn default() -> Self {
        Settings {
            host: Box::new(Console),
            container_engine: None,
            run_name: "iunctus".into(),
            command_line: String::new(),
        }
    }
}

/// A host that prints to standard output and reads local files only.
struct Console;

impl Host for Console {
    fn print(&mut self, text: &str) {
        print!("{text}");
    }
}

/// The Nextflow driver of a launch, with the default [`Settings`].
pub fn driver(launch: &Launch) -> Result<Box<dyn Driver>, DriverError> {
    Ok(Box::new(Nextflow::new(launch, Settings::default())?))
}

/// A run of a Nextflow script, from its launch to its end.
pub struct Nextflow {
    launch: Launch,
    settings: Settings,
    /// `None` until [`Driver::start`].
    run: Option<Run>,
    /// The tasks asked for that have not finished, by their id.
    requests: HashMap<TaskId, TaskRequest>,
    /// The variables of the config's `env` scope, which every task gets.
    environment: BTreeMap<String, String>,
}

impl Nextflow {
    pub fn new(launch: &Launch, settings: Settings) -> Result<Self, DriverError> {
        if let Some(target) = launch.targets.first() {
            return Err(DriverError::unsupported(format!(
                "choosing the entry workflow or a target like `{target}` is not supported yet"
            )));
        }
        Ok(Nextflow {
            launch: launch.clone(),
            settings,
            run: None,
            requests: HashMap::new(),
            environment: BTreeMap::new(),
        })
    }

    /// The container tasks of the requests of the network, which are kept until they finish.
    fn tasks(&mut self, requests: Vec<TaskRequest>) -> Vec<Task> {
        requests
            .into_iter()
            .map(|request| {
                let task = task(&request, &self.environment);
                self.requests.insert(request.id, request);
                task
            })
            .collect()
    }
}

impl Driver for Nextflow {
    fn language(&self) -> Language {
        Language::Nextflow
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        let host = self.settings.host.as_mut();
        // A project directory runs its `manifest.mainScript`, else `main.nf`. The parent of a
        // bare file name like `main.nf` is empty, which stands for the launch directory.
        let target = &self.launch.target;
        let project = if target.is_dir() {
            target.clone()
        } else {
            let parent = target
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty());
            parent.unwrap_or(Path::new(".")).to_owned()
        };
        let (config, files) = load_config(&project, &self.launch, host)?;
        let script = if target.is_dir() {
            let main = config
                .get(&["manifest", "mainScript"])
                .map_or("main.nf".into(), |name| name.to_string());
            target.join(main)
        } else {
            target.clone()
        };
        let module = Loader::default().load(&script).map_err(load_error)?;
        self.environment = environment(&config);
        let text = |path: &Path| {
            std::path::absolute(path)
                .unwrap_or_else(|_| path.to_owned())
                .to_string_lossy()
                .into_owned()
        };
        let profiles = &self.launch.profiles;
        let options = Options {
            stub: self.launch.stub,
            container_engine: self.settings.container_engine.clone(),
            config,
            profile: if profiles.is_empty() {
                "standard".into()
            } else {
                profiles.join(",")
            },
            project_dir: text(&project),
            script_file: text(&script),
            run_name: self.settings.run_name.clone(),
            config_files: (files.iter())
                .map(|file| file.to_string_lossy().into_owned())
                .collect(),
            command_line: self.settings.command_line.clone(),
        };
        let (run, requests) = Run::start(&module, &options, host).map_err(error)?;
        self.run = Some(run);
        Ok(self.tasks(requests))
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        let id = TaskId(outcome.id.0);
        let request = self.requests.remove(&id).ok_or_else(|| {
            DriverError::new(format!("the task {} was not asked for", outcome.id))
        })?;
        let mut outcome = outcome;
        if self.launch.assume_inputs {
            drop_made_up_outputs(&request, &mut outcome);
        }
        let outcome = task_outcome(&request, outcome)?;
        let run = self.run.as_mut().ok_or_else(not_started)?;
        let requests = run
            .complete(outcome, self.settings.host.as_mut())
            .map_err(error)?;
        Ok(self.tasks(requests))
    }

    fn finish(&mut self) -> Result<(), DriverError> {
        let run = self.run.as_mut().ok_or_else(not_started)?;
        run.finish(self.settings.host.as_mut()).map_err(error)
    }
}

/// In a simulated run, a task only pretends to run, and the simulator makes up the outputs that a
/// script does not tell how it makes. An output that a script, like a stub, surely does not
/// make is left out, as Nextflow's stub run finds no file for it: an optional output is then
/// missing and a required one fails the task, so the run takes the same path. An output that
/// names an input or the folder of one, without wildcards, keeps it, as Nextflow collects
/// such an input.
fn drop_made_up_outputs(request: &TaskRequest, outcome: &mut Outcome) {
    let inputs: Vec<&str> = request
        .staged
        .iter()
        .map(|(_, name)| name.as_str())
        .collect();
    let simulation = workflow_graph::simulator::shell::simulate(&request.script, &inputs);
    if !simulation.complete {
        return;
    }
    for pattern in &request.output_patterns {
        // A pattern like `./results/` names the folder `results`.
        let wanted = pattern.trim_start_matches("./").trim_end_matches('/');
        let literal = !wanted.contains(['*', '?', '[', '{']);
        // As Nextflow, only a pattern that starts with a dot matches hidden files.
        let hidden = if wanted.starts_with('.') {
            TASK_FILES
        } else {
            &[]
        };
        let made = (simulation.made.iter().map(|made| made.path.as_str()))
            .chain(hidden.iter().copied())
            .any(|path| glob_matches(wanted, path))
            || (literal && inputs.iter().any(|input| is_within(input, wanted)));
        if !made {
            outcome
                .files
                .retain(|file| !glob_matches(wanted, &file.path));
        }
    }
}

/// The hidden files that Nextflow writes into the work folder of every task, which outputs
/// like `.command.*` collect.
const TASK_FILES: &[&str] = &[
    ".command.begin",
    ".command.err",
    ".command.log",
    ".command.out",
    ".command.run",
    ".command.sh",
    ".exitcode",
];

/// Whether an input is the file or folder `wanted`, or inside that folder.
fn is_within(input: &str, wanted: &str) -> bool {
    input == wanted || input.starts_with(&format!("{wanted}/"))
}

fn not_started() -> DriverError {
    DriverError::new("the Nextflow run has not started")
}

fn error(error: Error) -> DriverError {
    DriverError {
        unsupported: error.unsupported(),
        message: error.to_string(),
    }
}

fn load_error(error: LoadError) -> DriverError {
    DriverError {
        unsupported: error.messages.iter().any(|message| {
            message.contains("not supported yet") || message.contains("not supported (decision")
        }),
        message: error.to_string(),
    }
}

/// The config of a project directory, as `nextflow run` and `nextflow config` read it.
pub fn config(project: &Path, launch: &Launch, host: &mut dyn Host) -> Result<Config, DriverError> {
    load_config(project, launch, host).map(|(config, _)| config)
}

/// The config of a project directory and the files it came from, in the order read.
fn load_config(
    project: &Path,
    launch: &Launch,
    host: &mut dyn Host,
) -> Result<(Config, Vec<PathBuf>), DriverError> {
    let launch_dir =
        std::env::current_dir().map_err(|error| DriverError::new(error.to_string()))?;
    let project = std::path::absolute(project).unwrap_or_else(|_| project.to_owned());
    let files = nextflow_config::files(&project, &launch_dir, &launch.configs);
    let options = nextflow_config::Options {
        profiles: (!launch.profiles.is_empty()).then(|| launch.profiles.clone()),
        params: params(launch)?,
        project_dir: project.to_string_lossy().into_owned(),
        launch_dir: launch_dir.to_string_lossy().into_owned(),
    };
    let config = nextflow_config::load(&files, &options, host).map_err(|error| DriverError {
        unsupported: error.unsupported(),
        message: error.to_string(),
    })?;
    Ok((config, files))
}

/// The params of a launch as nested maps: those of the params file (`-params-file`, JSON or
/// YAML), with the command-line params over them.
pub fn params(launch: &Launch) -> Result<Value, DriverError> {
    let params = match &launch.inputs {
        Some(file) => params_file(file)?,
        None => tree::map(),
    };
    for (name, text) in &launch.params {
        set_param(&params, name, text).map_err(DriverError::new)?;
    }
    Ok(params)
}

fn params_file(file: &Path) -> Result<Value, DriverError> {
    let text = std::fs::read_to_string(file)
        .map_err(|error| DriverError::new(format!("Cannot read {}: {error}", file.display())))?;
    let extension = file.extension().and_then(|extension| extension.to_str());
    let value = match extension {
        Some("json") => parse_json(&text),
        Some("yml" | "yaml") => parse_yaml(&text),
        _ => {
            return Err(DriverError::new(format!(
                "The params file {} must end with .json, .yml or .yaml",
                file.display()
            )));
        }
    };
    let invalid = |message: String| {
        DriverError::new(format!(
            "Cannot read the params file {}: {message}",
            file.display()
        ))
    };
    match value {
        Ok(value @ Value::Map(_)) => Ok(value),
        Ok(_) => Err(invalid("it holds no map of params".into())),
        Err(exception) => Err(invalid(exception.to_string())),
    }
}

/// `--a.b-c value` sets `params.a.bC`: dots nest, `\.` is a literal dot, and the last part
/// changes from kebab case to camel case.
fn set_param(params: &Value, name: &str, text: &str) -> Result<(), String> {
    let mut parts: Vec<String> = vec![String::new()];
    let mut characters = name.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\\' if characters.peek() == Some(&'.') => {
                characters.next();
                parts.last_mut().expect("a part").push('.');
            }
            '.' => parts.push(String::new()),
            other => parts.last_mut().expect("a part").push(other),
        }
    }
    let last = parts.pop().expect("a part");
    parts.push(camel_case(&last));
    let path: Vec<Rc<str>> = parts.iter().map(|part| part.as_str().into()).collect();
    tree::assign(params, &path, Value::string(text))
}

/// `kebab-case` becomes `kebabCase`.
fn camel_case(name: &str) -> String {
    let mut result = String::new();
    let mut upper = false;
    for character in name.chars() {
        match character {
            '-' => upper = true,
            other if upper => {
                result.extend(other.to_uppercase());
                upper = false;
            }
            other => result.push(other),
        }
    }
    result
}

/// The variables of the `env` scope of the config, which Nextflow exports in every task.
fn environment(config: &Config) -> BTreeMap<String, String> {
    let mut variables = BTreeMap::new();
    if let Some(Value::Map(map)) = config.get(&["env"]) {
        for (name, value) in &map.borrow().entries {
            variables.insert(name.0.to_string(), value.to_string());
        }
    }
    variables
}

/// The container task of a Nextflow task: `.command.sh`, with the code that captures `env` and
/// `eval` outputs, runs in the shell of the `shell` directive; the staged files are its inputs.
pub fn task(request: &TaskRequest, environment: &BTreeMap<String, String>) -> Task {
    let mut inputs = vec![Input {
        path: SCRIPT.into(),
        source: String::new(),
        directory: false,
        content: Some(command_script(request)),
    }];
    for (source, name) in &request.staged {
        let (source, directory) = match source.prefix {
            None => (source.path.clone(), Path::new(&source.path).is_dir()),
            Some(_) => (source.uri(), false),
        };
        inputs.push(Input {
            path: name.clone(),
            source,
            directory,
            content: None,
        });
    }
    let mut outputs = request.output_patterns.clone();
    if !request.env.is_empty() || !request.eval.is_empty() {
        outputs.push(ENVIRONMENT.into());
    }
    Task {
        id: workflow_graph::TaskId(request.id.0),
        name: request.name.clone(),
        step: request.process.to_string(),
        image: container(request),
        command: command(request),
        script: Some(request.script.clone()),
        workdir: None,
        environment: environment.clone(),
        inputs,
        outputs,
        stdin: None,
        resources: resources(request),
        after: Vec::new(),
    }
}

/// What the network reads of the outcome of a task: its output files, its standard output, and
/// the values of its `env` and `eval` outputs from `.command.env`. A value that a task did not
/// write, as with the simulator, is empty.
pub fn task_outcome(request: &TaskRequest, outcome: Outcome) -> Result<TaskOutcome, DriverError> {
    let mut result = TaskOutcome {
        id: request.id,
        exit_status: outcome.exit_code,
        stdout: outcome.stdout,
        ..TaskOutcome::default()
    };
    let mut variables = HashMap::new();
    for file in outcome.files {
        if file.path == ENVIRONMENT {
            let text = std::fs::read_to_string(&file.location).map_err(|error| {
                DriverError::new(format!(
                    "{}: cannot read {}: {error}",
                    request.name,
                    file.location.display()
                ))
            })?;
            variables = read_environment(&text);
            // The file is iunctus's own, unless an output asks for it.
            if !request.output_patterns.contains(&file.path) {
                continue;
            }
        }
        let path = PathValue::parse(&file.location.to_string_lossy(), "/");
        result.files.push((file.path, path));
    }
    for index in 0..request.eval.len() {
        let (value, status) = variables
            .get(&format!("nxf_out_eval_{}", index + 1))
            .cloned()
            .unwrap_or_default();
        if status != 0 {
            return Err(DriverError::new(format!(
                "{}: an `eval` output command failed with exit status {status}: {value}",
                request.name
            )));
        }
        result.eval.push(value);
    }
    for name in &request.env {
        let value = variables.get(&**name).map(|(value, _)| value.clone());
        result
            .env
            .insert(name.to_string(), value.unwrap_or_default());
    }
    Ok(result)
}
