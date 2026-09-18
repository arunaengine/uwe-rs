//! The Nextflow driver: a run of a script behind the language-neutral driver interface of
//! `workflow-graph`. It loads the config and the main script, starts the dataflow network,
//! turns each task the network asks for into a container task, and each outcome back.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use nextflow_config::{Config, tree};
use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, Host, parse_json, parse_yaml};
use nextflow_evaluator::lowered::Module;
use nextflow_evaluator::value::{PathValue, Value};
use workflow_graph::{
    Delivery, Driver, DriverError, Files, Input, Inputs, Language, Launch, Note, Outcome, Task,
};

use crate::notes::Noting;
use crate::process::glob_matches;
use crate::reports::Reports;
use crate::task::{
    ENVIRONMENT, SCRIPT, command, command_script, container, read_environment, resources,
    with_registry,
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
    /// What the run noted since the engine last asked: files it read, listed or spread.
    notes: Vec<Note>,
    /// The registry of the container engine, like `docker.registry`, for images without one.
    registry: Option<String>,
    /// The reports that the config turns on, like the trace, in a real run.
    reports: Option<Reports>,
    /// The deliveries of the reports, once they are written.
    report_deliveries: Vec<Delivery>,
    /// The config and the main script, once loaded; the run takes them when it starts.
    loaded: Option<Loaded>,
    /// The files of the tasks, where a real executor keeps them.
    files: Option<Arc<dyn Files>>,
    /// The files that `exec:` sections made, by their path, with the task that made them, so
    /// that the tasks that read them wait for it.
    made_by: HashMap<String, workflow_graph::TaskId>,
    acceptance: HashMap<(workflow_graph::TaskId, u32), bool>,
}

/// What a run needs before it starts, which [`Driver::inputs`] loads too.
struct Loaded {
    /// The project directory, where the main script and `nextflow.config` are.
    project: PathBuf,
    script: PathBuf,
    config: Config,
    /// The config files that were read, in order.
    files: Vec<PathBuf>,
    module: Rc<Module>,
}

impl Nextflow {
    pub fn new(launch: &Launch, settings: Settings) -> Result<Self, DriverError> {
        if launch.targets.len() > 1 {
            return Err(DriverError::new(
                "Only one Nextflow workflow entry may be selected",
            ));
        }
        Ok(Nextflow {
            launch: launch.clone(),
            settings,
            run: None,
            requests: HashMap::new(),
            environment: BTreeMap::new(),
            notes: Vec::new(),
            registry: None,
            reports: None,
            report_deliveries: Vec::new(),
            loaded: None,
            files: None,
            made_by: HashMap::new(),
            acceptance: HashMap::new(),
        })
    }

    /// Loads the config of the project and the main script, once.
    fn load(&mut self) -> Result<(), DriverError> {
        if self.loaded.is_some() {
            return Ok(());
        }
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
        if target.is_dir() && !script.is_file() {
            return Err(DriverError::new(format!(
                "{}: no such file; name the main script of the project, or set \
                 `manifest.mainScript` in its nextflow.config",
                script.display()
            )));
        }
        let module = Loader::default().load(&script).map_err(load_error)?;
        self.loaded = Some(Loaded {
            project,
            script,
            config,
            files,
            module,
        });
        Ok(())
    }

    /// The options of a run of the loaded script with a config.
    fn options(&self, loaded: &Loaded, config: Config) -> Options {
        let text = |path: &Path| {
            std::path::absolute(path)
                .unwrap_or_else(|_| path.to_owned())
                .to_string_lossy()
                .into_owned()
        };
        let profiles = &self.launch.profiles;
        Options {
            entry: self.launch.targets.first().cloned(),
            stub: self.launch.stub,
            simulated: self.launch.simulated,
            keep_going: self.launch.keep_going,
            output_dir: (self.launch.output_dir.as_ref())
                .map(|folder| folder.to_string_lossy().into_owned()),
            container_engine: self.settings.container_engine.clone(),
            config,
            profile: if profiles.is_empty() {
                "standard".into()
            } else {
                profiles.join(",")
            },
            project_dir: text(&loaded.project),
            script_file: text(&loaded.script),
            run_name: self.settings.run_name.clone(),
            config_files: (loaded.files.iter())
                .map(|file| file.to_string_lossy().into_owned())
                .collect(),
            command_line: self.settings.command_line.clone(),
            exec_folder: Some((self.launch.scratch.join("nextflow-exec")).join(format!(
                "{}-{}",
                std::process::id(),
                self.settings.run_name
            ))),
        }
    }

    /// The inputs of the loaded pipeline.
    fn describe(&mut self, loaded: &Loaded) -> Result<Inputs, DriverError> {
        let file = crate::inputs::schema_file(&loaded.config, &loaded.project);
        let from_file = file.is_file();
        let schema = match from_file {
            true => crate::inputs::from_file(&file, &loaded.project).map_err(DriverError::new)?,
            false => self.declared_schema(loaded)?,
        };
        // The params as the pipeline checks them: those of the config, with the `params.x =
        // value` statements of the script where the config leaves them unset, as Nextflow sets
        // them before the workflow runs. The run finds them set.
        let options = self.options(loaded, loaded.config.clone());
        let host = self.settings.host.as_mut();
        let params = crate::declared_params(&loaded.module, &options, host)
            .map_or_else(|_| loaded.config.params(), |(params, _)| params);
        let mut values = crate::inputs::values(&params);
        // The params that nf-schema ignores are left out of its check and of the schema on
        // purpose.
        let ignored: BTreeSet<String> =
            crate::inputs::ignored(&loaded.config).into_iter().collect();
        let mut read = crate::inputs::read(&loaded.project, &schema);
        read.retain(|name| !ignored.contains(name));
        if crate::inputs::lenient(&loaded.config) {
            crate::inputs::as_text(&schema, &mut values);
        }
        Ok(Inputs {
            schema,
            values,
            base: std::env::current_dir().unwrap_or_default(),
            read: Some(read),
            assumes_values: false,
            // Nextflow holds no param to a schema; a pipeline does when its code checks them.
            advisory: !(from_file && crate::inputs::validates(&loaded.module, &params)),
            ignored,
        })
    }

    /// The schema of a pipeline without a schema file, from what its config and main script
    /// declare without the params of the command line. A script whose declarations fail
    /// outside a run gives the params of the config alone.
    fn declared_schema(&mut self, loaded: &Loaded) -> Result<serde_json::Value, DriverError> {
        let mut bare = self.launch.clone();
        bare.params.clear();
        bare.inputs = None;
        let (config, _) = load_config(&loaded.project, &bare, self.settings.host.as_mut())?;
        let options = self.options(loaded, config.clone());
        let host = self.settings.host.as_mut();
        let (params, defaults) = crate::declared_params(&loaded.module, &options, host)
            .unwrap_or_else(|_| (config.params(), Vec::new()));
        let declared = crate::inputs::Declared {
            params,
            typed: &loaded.module.typed_parameters,
            defaults,
            title: title(&config, &loaded.project),
            description: manifest(&config, "description"),
        };
        let base = std::env::current_dir().unwrap_or_default();
        Ok(crate::inputs::declared(&declared, &base))
    }

    /// The container tasks of the requests of the network, which are kept until they finish.
    fn tasks(&mut self, requests: Vec<TaskRequest>) -> Result<Vec<Task>, DriverError> {
        requests
            .into_iter()
            .map(|request| {
                let mut task = task(&request, &self.environment)?;
                let registry = self.registry.as_deref();
                task.image = (task.image).map(|image| with_registry(&image, registry));
                for input in &task.inputs {
                    if let Some(maker) = self.made_by.get(&input.source)
                        && !task.after.contains(maker)
                    {
                        task.after.push(*maker);
                    }
                }
                for (_, path) in request.made.iter().flatten() {
                    self.made_by.insert(path.path.clone(), task.id);
                }
                if let Some(reports) = &mut self.reports {
                    reports.asked(&task);
                }
                self.requests.insert(request.id, request);
                Ok(task)
            })
            .collect()
    }
}

impl Driver for Nextflow {
    fn language(&self) -> Language {
        Language::Nextflow
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        self.load()?;
        let loaded = self.loaded.take().ok_or_else(not_started)?;
        let config = &loaded.config;
        self.environment = environment(config);
        let engine = self
            .settings
            .container_engine
            .as_deref()
            .unwrap_or("docker");
        self.registry = config
            .get(&[engine, "registry"])
            .map(|registry| registry.to_string());
        if !self.launch.simulated {
            let launch_dir = std::env::current_dir().unwrap_or_default();
            let script = loaded.script.to_string_lossy();
            let reports = Reports::of(config, &launch_dir, &script);
            self.reports = Some(reports).filter(|reports| !reports.is_empty());
        }
        let options = self.options(&loaded, loaded.config.clone());
        let mut host = Noting {
            host: self.settings.host.as_mut(),
            notes: &mut self.notes,
        };
        let (run, requests) = Run::start_retained(&loaded.module, &options, &mut host);
        self.run = Some(run);
        self.tasks(requests.map_err(error)?)
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        let identity = (outcome.id, outcome.attempt);
        let id = TaskId(outcome.id.0);
        let request = self.requests.remove(&id).ok_or_else(|| {
            DriverError::new(format!("the task {} was not asked for", outcome.id))
        })?;
        if let Some(reports) = &mut self.reports {
            reports.ended(&outcome);
        }
        let mut outcome = outcome;
        let outcome = match &request.made {
            // An `exec:` section ran when the task was asked for; its files are its outputs.
            Some(made) if outcome.exit_code == 0 => TaskOutcome {
                id: request.id,
                files: made.clone(),
                ..TaskOutcome::default()
            },
            _ => {
                if self.launch.simulated {
                    drop_made_up_outputs(&request, &mut outcome);
                }
                task_outcome_as(
                    &request,
                    outcome,
                    self.files.as_deref(),
                    self.launch.simulated,
                )?
            }
        };
        let run = self.run.as_mut().ok_or_else(not_started)?;
        let mut host = Noting {
            host: self.settings.host.as_mut(),
            notes: &mut self.notes,
        };
        let completed = run.complete(outcome, &mut host);
        self.acceptance
            .insert(identity, run.take_acceptance(request.id));
        let requests = completed.map_err(error)?;
        self.tasks(requests)
    }

    fn take_acceptance(&mut self, task: workflow_graph::TaskId, attempt: u32) -> bool {
        self.acceptance.remove(&(task, attempt)).unwrap_or(false)
    }

    fn draining(&self) -> bool {
        self.run.as_ref().is_some_and(Run::draining)
    }

    fn finish(&mut self) -> Result<(), DriverError> {
        let run = self.run.as_mut().ok_or_else(not_started)?;
        run.finish(self.settings.host.as_mut()).map_err(error)?;
        if let Some(reports) = &self.reports {
            // The reports go to a folder of this run, from where the engine delivers them.
            let run = format!("{}-{}", std::process::id(), self.settings.run_name);
            let folder = self.launch.scratch.join("nextflow-reports").join(run);
            self.report_deliveries = reports.write(&folder).map_err(|error| {
                DriverError::new(format!("cannot write the reports of the run: {error}"))
            })?;
        }
        Ok(())
    }

    fn failed(&mut self, failure: &workflow_graph::ExecuteError) -> Result<(), DriverError> {
        let Some(run) = &mut self.run else {
            return Ok(());
        };
        let cancelled = matches!(failure, workflow_graph::ExecuteError::Cancelled);
        run.failed(
            &failure.to_string(),
            if cancelled { 130 } else { 1 },
            matches!(failure, workflow_graph::ExecuteError::Driver(_)),
            self.settings.host.as_mut(),
        )
        .map_err(error)
    }

    fn notes(&mut self) -> Vec<Note> {
        std::mem::take(&mut self.notes)
    }

    fn deliveries(&mut self) -> Vec<Delivery> {
        let mut deliveries = match self.run.as_mut() {
            Some(run) => run.take_deliveries(),
            None => Vec::new(),
        };
        deliveries.append(&mut self.report_deliveries);
        deliveries
    }

    /// The pipeline's `nextflow_schema.json`, else what its config and script declare, with
    /// the params of the run and the params that its code reads.
    fn inputs(&mut self) -> Result<Option<Inputs>, DriverError> {
        self.load()?;
        let loaded = self.loaded.take().ok_or_else(not_started)?;
        let inputs = self.describe(&loaded);
        self.loaded = Some(loaded);
        inputs.map(Some)
    }

    fn use_inputs(&mut self, values: serde_json::Value) -> Result<(), DriverError> {
        self.load()?;
        let text =
            serde_json::to_string(&values).map_err(|error| DriverError::new(error.to_string()))?;
        let values = parse_json(&text).map_err(|error| DriverError::new(error.to_string()))?;
        let loaded = self.loaded.as_mut().ok_or_else(not_started)?;
        tree::deep_merge(&loaded.config.params(), &values);
        Ok(())
    }

    fn use_files(&mut self, files: Arc<dyn Files>) {
        self.files = Some(files);
    }

    /// The outputs of the `output { }` block of the main script, which it publishes into the
    /// output folder (`-output-dir`, else `outputDir` of the config, else `results`), else the
    /// `emit`s of the workflows of the main script.
    fn outputs(&mut self) -> Result<Option<serde_json::Value>, DriverError> {
        self.load()?;
        let loaded = self.loaded.take().ok_or_else(not_started)?;
        let options = self.options(&loaded, loaded.config.clone());
        let host = self.settings.host.as_mut();
        let targets = crate::output_targets(&loaded.module, &options, host);
        let output_dir = (self.launch.output_dir.as_ref())
            .map(|folder| folder.to_string_lossy().into_owned())
            .or_else(|| {
                let folder = loaded.config.get(&["outputDir"]).filter(Value::truth);
                folder.map(|folder| folder.to_string())
            })
            .unwrap_or_else(|| "results".into());
        let title = title(&loaded.config, &loaded.project);
        let schema = crate::output_schema::schema(&loaded.module, &targets, &output_dir, &title);
        self.loaded = Some(loaded);
        Ok(Some(schema))
    }
}

/// A setting of the manifest of the config, like its `name`.
fn manifest(config: &Config, key: &str) -> Option<String> {
    (config.get(&["manifest", key]))
        .filter(Value::truth)
        .map(|value| value.to_string())
}

/// The name of a pipeline: the name in its manifest, else the name of its folder.
fn title(config: &Config, project: &Path) -> String {
    manifest(config, "name").unwrap_or_else(|| {
        let project = std::path::absolute(project).unwrap_or_default();
        let name = project.file_name().unwrap_or_default();
        name.to_string_lossy().into_owned()
    })
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
pub fn task(
    request: &TaskRequest,
    environment: &BTreeMap<String, String>,
) -> Result<Task, DriverError> {
    let reuse = match request
        .directives
        .iter()
        .rev()
        .find(|directive| matches!(&*directive.name, "cache" | "cacheable"))
        .and_then(|directive| directive.arguments.first())
    {
        Some(Value::Boolean(false)) => workflow_graph::Reuse::Disabled,
        _ if request.attempt > 1 => workflow_graph::Reuse::Denied,
        _ => workflow_graph::Reuse::Allowed,
    };
    let mut environment = environment.clone();
    environment.extend(
        request
            .input_environment
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string())),
    );
    if request.made.is_some() {
        return Ok(exec_task(request, environment, reuse));
    }
    let resources = resources(request).map_err(DriverError::new)?;
    let mut inputs = vec![Input {
        path: SCRIPT.into(),
        source: String::new(),
        directory: false,
        content: Some(command_script(request)),
        ..Default::default()
    }];
    let writable = match crate::task::copies_inputs(request) {
        true => workflow_graph::Writable::Copy,
        false => workflow_graph::Writable::No,
    };
    for (source, name) in &request.staged {
        let (source, directory) = match source.prefix {
            None => (source.path.clone(), Path::new(&source.path).is_dir()),
            Some(_) => (source.uri(), false),
        };
        inputs.push(Input {
            executable: false,
            path: name.clone(),
            source,
            directory,
            content: None,
            writable,
        });
    }
    let mut outputs = request.output_patterns.clone();
    if !request.env.is_empty() || !request.eval.is_empty() {
        outputs.push(ENVIRONMENT.into());
    }
    Ok(Task {
        run_as: None,
        id: workflow_graph::TaskId(request.id.0),
        name: request.name.clone(),
        step: request.process.to_string(),
        image: container(request),
        command: command(request),
        script: Some(request.script.clone()),
        workdir: None,
        environment,
        inputs,
        outputs,
        stdin: request.stdin.as_ref().map(Value::to_string),
        resources,
        network: workflow_graph::Network::Unstated,
        after: Vec::new(),
        containers: Vec::new(),
        replaces_entrypoint: false,
        reuse,
    })
}

/// What the network reads of the outcome of a task: its output files, its standard output, and
/// the values of its `env` and `eval` outputs from `.command.env`, which it reads through
/// `files` when the run has them. A value that a task did not write, as with the simulator, is
/// empty.
/// The task of an `exec:` section, which ran in the interpreter when it was asked for (decision
/// 17): it has no image and no command, so the engine finishes it at once (decision 11 asks
/// no container of it), and it stands in the graph with the files it read.
fn exec_task(
    request: &TaskRequest,
    environment: BTreeMap<String, String>,
    reuse: workflow_graph::Reuse,
) -> Task {
    let inputs = (request.staged.iter())
        .map(|(source, name)| Input {
            path: name.clone(),
            source: match source.prefix {
                None => source.path.clone(),
                Some(_) => source.uri(),
            },
            directory: false,
            content: None,
            ..Default::default()
        })
        .collect();
    Task {
        run_as: None,
        id: workflow_graph::TaskId(request.id.0),
        name: request.name.clone(),
        step: request.process.to_string(),
        image: None,
        command: Vec::new(),
        script: None,
        workdir: None,
        environment,
        inputs,
        outputs: Vec::new(),
        stdin: request.stdin.as_ref().map(Value::to_string),
        resources: workflow_graph::Resources::default(),
        network: workflow_graph::Network::Denied,
        after: Vec::new(),
        containers: Vec::new(),
        replaces_entrypoint: false,
        reuse,
    }
}

pub fn task_outcome(
    request: &TaskRequest,
    outcome: Outcome,
    files: Option<&dyn Files>,
) -> Result<TaskOutcome, DriverError> {
    task_outcome_as(request, outcome, files, false)
}

fn task_outcome_as(
    request: &TaskRequest,
    outcome: Outcome,
    files: Option<&dyn Files>,
    simulated: bool,
) -> Result<TaskOutcome, DriverError> {
    let mut result = TaskOutcome {
        id: request.id,
        exit_status: outcome.exit_code,
        stdout: outcome.stdout,
        ..TaskOutcome::default()
    };
    let mut variables = HashMap::new();
    for file in outcome.files {
        if file.path == ENVIRONMENT {
            let bytes = match workflow_graph::files::read(files, &file.location) {
                Ok(bytes) => bytes,
                Err(error) => {
                    result.failure = Some(Error::Thrown {
                        exception: Rc::new(Exception::new(
                            "IOException",
                            format!("{}: cannot read {}: {error}", request.name, file.location),
                        )),
                        span: Default::default(),
                    });
                    return Ok(result);
                }
            };
            variables = read_environment(&String::from_utf8_lossy(&bytes));
            // The file is iunctus's own, unless an output asks for it.
            if !request.output_patterns.contains(&file.path) {
                continue;
            }
        }
        let path = PathValue::parse(&file.location, "/");
        result.files.push((file.path, path));
    }
    for index in 0..request.eval.len() {
        let (value, status) = match variables
            .get(&format!("nxf_out_eval_{}", index + 1))
            .cloned()
        {
            Some(record) => record,
            None if simulated => (String::new(), 0),
            None => {
                result.failure.get_or_insert_with(|| Error::Thrown {
                    exception: Rc::new(Exception::new(
                        "ProcessFailedException",
                        format!(
                            "{}: missing capture for `eval` output command {}",
                            request.name,
                            index + 1
                        ),
                    )),
                    span: Default::default(),
                });
                continue;
            }
        };
        if status != 0 {
            result.failure.get_or_insert_with(|| Error::Thrown {
                exception: Rc::new(Exception::new(
                    "ProcessFailedException",
                    format!(
                        "{}: an `eval` output command failed with exit status {status}: {value}",
                        request.name
                    ),
                )),
                span: Default::default(),
            });
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
