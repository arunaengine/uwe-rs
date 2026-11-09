//! Runs Nextflow dataflow through workflow-graph, mapping task requests and outcomes.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use nextflow_config::{Config, tree};
use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, Host, parse_json, parse_yaml};
use nextflow_evaluator::lowered::Module;
use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::{PathValue, Value};
use nextflow_plugins::observer::Observer;
use workflow_graph::{
    Delivery, DeliveryMode, Driver, DriverError, Files, Input, Inputs, Language, Launch, Note,
    Outcome, Task,
};

use crate::calls::Calls;
use crate::notes::Noting;
use crate::process::glob_matches;
use crate::reports::Reports;
use crate::task::{
    ENVIRONMENT, SCRIPT, command, command_script, container, launcher, read_environment, resources,
    wave_sources, with_registry,
};
use crate::{LoadError, Loader, Options, Run, TaskId, TaskOutcome, TaskRequest};

/// The folder of a run's own files in the scratch folder: what `exec:` sections and operators
/// make, and the reports. Its name starts with the process, then the run, which `uwe clean`
/// checks; the driver removes it when it goes.
pub fn run_folder(scratch: &Path, run: &str) -> PathBuf {
    scratch
        .join("nextflow")
        .join(format!("{}-{run}", std::process::id()))
}

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
    pub secret_env: bool,
    pub local_options: bool,
    /// Where uwe pulled the pipeline from, which run observers hear.
    pub repository: Option<Repository>,
    /// Whether the run resumes an earlier one, `workflow.resume`.
    pub resume: bool,
}

/// The Git repository of a pulled pipeline, as `workflow.repository`, `workflow.commitId` and
/// `workflow.revision` describe it.
#[derive(Clone, Debug, Default)]
pub struct Repository {
    pub url: String,
    /// `owner/name`, as `workflow.projectName` shows it.
    pub name: String,
    pub commit_id: Option<String>,
    /// The revision asked for, like a branch or tag.
    pub revision: Option<String>,
}

impl Default for Settings {
    /// Prints to standard output and reads local files only.
    fn default() -> Self {
        Settings {
            host: Box::new(Console),
            container_engine: None,
            run_name: "uwe".into(),
            command_line: String::new(),
            secret_env: false,
            local_options: false,
            repository: None,
            resume: false,
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
    /// What nf-labrobotics adds to the tasks of its executor, when the config names it.
    lab: Option<nextflow_plugins::Lab>,
    project_bin: Option<PathBuf>,
    /// The names of the files in `project_bin`, which task scripts call by name.
    bin_scripts: Vec<String>,
    /// What the run noted since the engine last asked: files it read, listed or spread.
    notes: Vec<Note>,
    /// The registry of the container engine, like `docker.registry`, for images without one.
    registry: Option<String>,
    /// The image map of nf-biffix, which every container goes through.
    images: Option<nextflow_plugins::ImageMap>,
    conda_channels: Vec<String>,
    /// The Wave service of nf-wave, when the config turns it on.
    wave: Option<nextflow_plugins::Wave>,
    /// The reports that the config turns on, like the trace, in a real run.
    reports: Option<Reports>,
    /// The deliveries of the reports, once they are written.
    report_deliveries: Vec<Delivery>,
    /// The message of the exception that failed the run, which `workflow.errorMessage` shows.
    cause: Option<String>,
    /// The config and the main script, once loaded; the run takes them when it starts.
    loaded: Option<Loaded>,
    /// The files of the tasks, where a real executor keeps them.
    files: Option<Arc<dyn Files>>,
    /// The files that `exec:` sections made, by their path, with the task that made them, so
    /// that the tasks that read them wait for it.
    made_by: HashMap<String, workflow_graph::TaskId>,
    acceptance: HashMap<(workflow_graph::TaskId, u32), bool>,
    /// The run observers that the caller adds; a real run joins them to those of the plugins.
    observers: Vec<Box<dyn Observer>>,
    /// The model calls of agent tasks, which run before the engine records their tasks.
    calls: Calls,
}

/// A finished attempt, with its request and the outcome that the observers hear.
pub(crate) struct Ended {
    pub request: TaskRequest,
    pub outcome: TaskOutcome,
    pub heard: Option<Outcome>,
}

/// What a run needs before it starts, which [`Driver::inputs`] loads too.
pub(crate) struct Loaded {
    /// The project directory, where the main script and `nextflow.config` are.
    project: PathBuf,
    script: PathBuf,
    pub config: Config,
    /// The config files that were read, in order.
    files: Vec<PathBuf>,
    pub module: Rc<Module>,
    /// Whether the run reads command-line params as numbers and booleans, as Nextflow 26.04.6
    /// does with `NXF_SYNTAX_PARSER=v1`, which scripts with legacy syntax need.
    legacy: bool,
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
            lab: None,
            project_bin: None,
            bin_scripts: Vec::new(),
            notes: Vec::new(),
            registry: None,
            images: None,
            conda_channels: vec!["conda-forge".into(), "bioconda".into()],
            wave: None,
            reports: None,
            report_deliveries: Vec::new(),
            cause: None,
            loaded: None,
            files: None,
            made_by: HashMap::new(),
            acceptance: HashMap::new(),
            observers: Vec::new(),
            calls: Calls::default(),
        })
    }

    /// Adds an observer that hears the events of a real run, besides those of the plugins.
    pub fn observe(&mut self, observer: Box<dyn Observer>) {
        self.observers.push(observer);
    }

    /// Loads the config of the project and the main script, once.
    fn load(&mut self) -> Result<(), DriverError> {
        if self.loaded.is_some() {
            return Ok(());
        }
        let host = self.settings.host.as_mut();
        let target = &self.launch.target;
        let project = project_folder(target);
        let (mut config, mut files) = load_config(&project, &self.launch, host, false)?;
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
        let mut loader = Loader::default();
        let module = loader.load(&script).map_err(load_error)?;
        // Nextflow reads the params of the command line as numbers and booleans with the legacy
        // parser, which `NXF_SYNTAX_PARSER=v1` selects and legacy syntax needs.
        let parser = host.environment("NXF_SYNTAX_PARSER");
        let disabled = host.environment("NXF_DISABLE_PARAMS_TYPE_DETECTION");
        let legacy = (loader.legacy() || parser.as_deref() == Some("v1"))
            && disabled.is_none_or(|value| value.is_empty());
        // The config sees the params of the command line as the script does.
        if legacy && !self.launch.params.is_empty() {
            (config, files) = load_config(&project, &self.launch, host, true)?;
        }
        self.loaded = Some(Loaded {
            project,
            script,
            config,
            files,
            module,
            legacy,
        });
        Ok(())
    }

    /// The options of a run of the loaded script with a config.
    pub(crate) fn options(&self, loaded: &Loaded, config: Config) -> Options {
        let text = |path: &Path| {
            std::path::absolute(path)
                .unwrap_or_else(|_| path.to_owned())
                .to_string_lossy()
                .into_owned()
        };
        let profiles = &self.launch.profiles;
        let cli_params = match params(&self.launch, false) {
            Ok(Value::Map(map)) => (map.borrow().entries.keys())
                .map(|name| name.0.to_string())
                .collect(),
            _ => Vec::new(),
        };
        Options {
            cli_params,
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
            resume: self.settings.resume,
            exec_folder: Some(self.folder().join("exec")),
            cleanup: (!self.launch.simulated && nextflow_plugins::cleanup(&loaded.config))
                .then(|| std::path::absolute(self.folder()).unwrap_or_else(|_| self.folder())),
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
        let mut values = if crate::inputs::schema_validation(&loaded.module) {
            crate::inputs::to_json(&nextflow_plugins::schema_params(&params, &loaded.config))
        } else {
            crate::inputs::values(&params)
        };
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
            base: launch_folder(self.settings.host.as_ref()),
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
        let host = self.settings.host.as_mut();
        let (config, _) = load_config(&loaded.project, &bare, host, loaded.legacy)?;
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
        let base = launch_folder(self.settings.host.as_ref());
        Ok(crate::inputs::declared(&declared, &base))
    }

    /// The container tasks of the requests of the network, which are kept until they finish.
    pub(crate) fn tasks(&mut self, requests: Vec<TaskRequest>) -> Result<Vec<Task>, DriverError> {
        let host = self.settings.host.as_mut();
        let mut asked = Vec::new();
        for request in requests {
            asked.extend(self.calls.ask(request, host).map_err(DriverError::new)?);
        }
        asked
            .into_iter()
            .map(|request| {
                let names = crate::task::secrets(&request.directives).map_err(error)?;
                if !names.is_empty() && !self.settings.secret_env {
                    return Err(DriverError {
                        unsupported: true,
                        message: "process secret injection requires the local executor".into(),
                    });
                }
                let mut task = task(&request, &self.environment)?;
                // The task of an `exec:` section or an agent ran in the driver; it has no command.
                let driven = request.made.is_some() || request.agent.is_some();
                if let Some(lab) = self.lab.as_ref().filter(|_| !driven) {
                    lab.apply(&request.directives, &mut task.environment);
                }
                if task.environment.contains_key(crate::options::WORK_ENV) && !self.settings.local_options {
                    return Err(DriverError { unsupported: true, message: "container PWD environment binding requires the local executor".into() });
                }
                if let Some(source) = task.image.as_deref().and_then(|image| image.strip_prefix("conda-env:nextflow-packages:")) {
                    let source: String = serde_json::from_str(source).map_err(|_| DriverError::new("invalid Conda package reference"))?;
                    task.image = Some(format!("conda-env:nextflow-packages:{}", serde_json::json!({"packages": source, "channels": self.conda_channels})));
                }
                if let Some(flags) = task.environment.get(crate::options::FLAGS_ENV) && !self.settings.local_options {
                    let flags: BTreeMap<String, i64> = serde_json::from_str(flags).map_err(|_| DriverError { unsupported: true, message: "invalid container launch flags".into() })?;
                    if flags.iter().all(|(name, count)| name == "cpus" || name == "gpus" && *count >= 0) {
                        task.environment.remove(crate::options::FLAGS_ENV);
                    } else {
                        return Err(DriverError { unsupported: true, message: "container shared memory, swap and GPU 'all' flags require the local executor".into() });
                    }
                }
                if !driven
                    && let Some(bin) = &self.project_bin
                    && names_bin_script(&task, &self.bin_scripts)
                {
                    task.inputs.push(Input {
                        path: ".command.bin".into(),
                        source: bin.to_string_lossy().into_owned(),
                        directory: true,
                        ..Default::default()
                    });
                    task.inputs.push(Input {
                        path: ".command.run".into(),
                        content: Some(
                            "export PATH=\"$PWD/.command.bin:$PATH\"\nexec \"$@\"\n".into(),
                        ),
                        ..Default::default()
                    });
                    task.command
                        .splice(0..0, ["/bin/sh".into(), ".command.run".into()]);
                }
                let registry = self.registry.as_deref();
                let written = task.image.take();
                task.image = (written.as_deref()).map(|image| with_registry(image, registry));
                if let (Some(images), Some(written), Some(image)) =
                    (&self.images, &written, &task.image)
                {
                    let mapped = images.resolve(&request.process, written, image);
                    task.image = Some(mapped.map_err(DriverError::new)?);
                }
                if let Some(wave) = self.wave.as_mut().filter(|_| !driven) {
                    let sources = wave_sources(&request, registry, &self.conda_channels);
                    if let Some(image) = wave.resolve(&sources).map_err(DriverError::new)? {
                        task.image = Some(image);
                    }
                }
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
                    reports.asked(&task, &request);
                }
                self.requests.insert(request.id, request);
                Ok(task)
            })
            .collect()
    }

    /// Loads the run and sets up what every task of it gets, like the `env` scope, the
    /// registry and the `bin` folder, without starting its network.
    pub(crate) fn setup(&mut self) -> Result<Loaded, DriverError> {
        self.load()?;
        let loaded = self.loaded.take().ok_or_else(not_started)?;
        let bin = loaded.project.join("bin");
        let entries = std::fs::read_dir(&bin).into_iter().flatten().flatten();
        let files = entries.filter(|entry| entry.file_type().is_ok_and(|kind| !kind.is_dir()));
        self.bin_scripts = files
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        self.project_bin = bin.is_dir().then_some(bin);
        let config = &loaded.config;
        if config
            .get(&["conda", "enabled"])
            .is_some_and(|value| value.truth())
            && let Some(value) = config.get(&["conda", "channels"]).filter(Value::truth)
        {
            self.conda_channels = match value {
                Value::List(list) => list.borrow().items.iter().map(Value::to_string).collect(),
                Value::String(text) | Value::GString(text) => text
                    .split(',')
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(str::to_owned)
                    .collect(),
                _ => {
                    return Err(DriverError::new(
                        "conda.channels expects a list or comma-separated text",
                    ));
                }
            };
        }
        self.environment = environment(config);
        let answers = self.launch.scratch.join("nextflow").join("agents");
        self.calls = Calls::new(config, answers, self.settings.resume, self.launch.simulated);
        self.lab = nextflow_plugins::labrobotics(config, &self.settings.run_name);
        let engine = self
            .settings
            .container_engine
            .as_deref()
            .unwrap_or("docker");
        self.registry = config
            .get(&[engine, "registry"])
            .map(|registry| registry.to_string());
        let launch_dir = launch_folder(self.settings.host.as_ref());
        let host = &self.settings.host;
        let variable = |name: &str| host.environment(name);
        self.images = nextflow_plugins::image_map(config, &variable);
        self.wave = nextflow_plugins::wave(config, &variable, self.launch.simulated)
            .map_err(DriverError::new)?;
        if let Some(note) = self.wave.as_ref().and_then(nextflow_plugins::Wave::note) {
            self.settings.host.print(&format!("WARN: {note}\n"));
        }
        if !self.launch.simulated {
            for warning in nextflow_plugins::catalogue::warnings(config) {
                self.settings.host.print(&format!("WARN: {warning}\n"));
            }
            let host = &self.settings.host;
            let variable = |name: &str| host.environment(name);
            let observers = nextflow_plugins::catalogue::observers(config, &launch_dir, &variable);
            let script = loaded.script.to_string_lossy();
            let mut reports = Reports::of(config, &launch_dir, &script);
            reports.observe(observers.map_err(DriverError::new)?);
            reports.observe(std::mem::take(&mut self.observers));
            self.reports = Some(reports).filter(|reports| !reports.is_empty());
        }
        Ok(loaded)
    }

    /// A finished attempt as its process reads it: the files, the standard output and the
    /// `env` and `eval` values, or the files that an `exec:` section made.
    pub(crate) fn ended(&mut self, outcome: Outcome) -> Result<Ended, DriverError> {
        let id = TaskId(outcome.id.0);
        let request = self.requests.remove(&id).ok_or_else(|| {
            DriverError::new(format!("the task {} was not asked for", outcome.id))
        })?;
        let mut outcome = outcome;
        // The observers hear how the attempt ended once its outputs were checked, with whole
        // streams from the store: the outcome of a failed task keeps only their end.
        let heard = self.reports.as_ref().map(|reports| {
            let mut heard = outcome.clone();
            let whole = |log: &Option<workflow_graph::Stream>| {
                (log.as_ref()).and_then(|log| stdout_text(self.files.as_deref(), log).ok())
            };
            if reports.observed() {
                heard.stderr = whole(&heard.stderr_log).unwrap_or(heard.stderr);
                heard.stdout = whole(&heard.stdout_log).unwrap_or(heard.stdout);
            }
            heard
        });
        let outcome = match &request.made {
            _ if request.agent.is_some() => self.calls.outcome(&request),
            // An `exec:` section ran when the task was asked for; its files are its outputs.
            Some(made) if outcome.exit_code == 0 => TaskOutcome {
                id: request.id,
                files: made.clone(),
                ..TaskOutcome::default()
            },
            _ => {
                if self.launch.simulated {
                    drop_simulated_outputs(&request, &mut outcome);
                }
                task_outcome_as(
                    &request,
                    outcome,
                    self.files.as_deref(),
                    self.launch.simulated,
                )?
            }
        };
        Ok(Ended {
            request,
            outcome,
            heard,
        })
    }

    /// The network that the workflow builds, without starting it, for the compiler into wir.
    pub fn export(&mut self) -> Result<crate::export::Network, DriverError> {
        // The values that the config and the scripts read from the environment stay out of
        // the network; they are noted for this compile alone.
        nextflow_evaluator::secrets::noting(|| {
            let loaded = self.setup()?;
            let options = self.options(&loaded, loaded.config.clone());
            let host = self.settings.host.as_mut();
            let network = crate::export(&loaded.module, &options, host);
            self.loaded = Some(loaded);
            network.map_err(|failure| self.fault(failure))
        })
    }

    /// The params of the launch and config with the schema of the pipeline and the launch
    /// folder, as an empty network has them, for a pipeline whose network cannot be exported.
    pub fn interface(&mut self) -> Result<crate::export::Network, DriverError> {
        let loaded = self.setup()?;
        let options = self.options(&loaded, loaded.config.clone());
        self.loaded = Some(loaded);
        // Each param that crosses into wir is an input; one that cannot, like a duration,
        // keeps the default of the config, which the native op reads too.
        let mut params = serde_json::Map::new();
        if let Value::Map(map) = options.config.params() {
            for (key, value) in &map.borrow().entries {
                if let Ok(value) = crate::bridge::to_json(value) {
                    params.insert(key.0.to_string(), value);
                }
            }
        }
        let params = serde_json::Value::Object(params);
        let project = Path::new(&options.project_dir);
        let schema = crate::inputs::schema_file(&options.config, project);
        let schema = (schema.is_file()).then(|| crate::inputs::from_file(&schema, project).ok());
        Ok(crate::export::Network {
            params,
            schema: schema.flatten(),
            launch: launch_folder(self.settings.host.as_ref())
                .to_string_lossy()
                .into_owned(),
            ..crate::export::Network::default()
        })
    }

    /// The host of the run, which prints and reads files.
    pub(crate) fn host(&mut self) -> &mut dyn Host {
        self.settings.host.as_mut()
    }

    /// Starts the reports of a run whose tasks the adapter asks for, with its processes.
    pub(crate) fn reports_started(
        &mut self,
        options: &Options,
        processes: Vec<nextflow_plugins::observer::Process>,
    ) {
        let launch_dir = launch_folder(self.settings.host.as_ref());
        if let Some(reports) = &mut self.reports {
            let repository = self.settings.repository.as_ref();
            reports.run_started(options, &launch_dir, processes, repository);
        }
    }

    /// Tells the reports how an attempt ended, once its outputs were checked.
    pub(crate) fn reports_ended(&mut self, heard: Option<Outcome>, accepted: bool) {
        if let (Some(reports), Some(heard)) = (&mut self.reports, heard) {
            reports.ended(&heard, accepted);
        }
    }

    /// Ends the reports of a run without a network, with the error that failed it, and writes
    /// them; a run that succeeds checks them first, as [`Driver::finish`] does.
    pub(crate) fn reports_finished(&mut self, error: Option<&str>) -> Result<(), DriverError> {
        if let Some(reports) = &mut self.reports {
            if error.is_none() {
                reports.check().map_err(DriverError::new)?;
            }
            reports.run_ended(error, None, Vec::new());
        }
        self.write_reports()
    }

    /// Writes report artifacts for either successful or failed runs.
    fn write_reports(&mut self) -> Result<(), DriverError> {
        if let Some(reports) = &self.reports {
            // The reports go to the folder of this run, from where the engine delivers them.
            let folder = self.folder().join("reports");
            self.report_deliveries = reports.write(&folder).map_err(|error| {
                DriverError::new(format!("cannot write the reports of the run: {error}"))
            })?;
        }
        Ok(())
    }

    fn folder(&self) -> PathBuf {
        run_folder(&self.launch.scratch, &self.settings.run_name)
    }

    /// The error of a failed run, remembering the message of its first exception.
    fn fault(&mut self, failure: Error) -> DriverError {
        if let Some(exception) = failure.exception() {
            self.cause.get_or_insert_with(|| exception.message.clone());
        }
        error(failure)
    }

    /// What the entry workflow emitted so far, by emit name: a value channel gives its value,
    /// a queue channel the array of its values. `None` before the run started.
    pub fn emitted(&self) -> Result<Option<serde_json::Value>, DriverError> {
        let Some(run) = &self.run else {
            return Ok(None);
        };
        run.emitted().map(Some).map_err(DriverError::new)
    }
}

/// The engine has run and delivered everything once the driver goes.
impl Drop for Nextflow {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.folder());
    }
}

impl Driver for Nextflow {
    fn language(&self) -> Language {
        Language::Nextflow
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        let loaded = self.setup()?;
        let launch_dir = launch_folder(self.settings.host.as_ref());
        let options = self.options(&loaded, loaded.config.clone());
        let mut host = Noting {
            host: self.settings.host.as_mut(),
            notes: &mut self.notes,
        };
        let (run, requests) = Run::start_retained(&loaded.module, &options, &mut host);
        // Observers hear the start once the network is built, so they get the process graph.
        if let Some(reports) = &mut self.reports {
            let repository = self.settings.repository.as_ref();
            reports.run_started(&options, &launch_dir, run.processes(), repository);
        }
        self.run = Some(run);
        let requests = requests.map_err(|failure| self.fault(failure))?;
        self.tasks(requests)
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        let identity = (outcome.id, outcome.attempt);
        let Ended {
            request,
            outcome,
            heard,
        } = self.ended(outcome)?;
        let run = self.run.as_mut().ok_or_else(not_started)?;
        let mut host = Noting {
            host: self.settings.host.as_mut(),
            notes: &mut self.notes,
        };
        let completed = run.complete(outcome, &mut host);
        let accepted = run.take_acceptance(request.id);
        self.acceptance.insert(identity, accepted);
        if let (Some(reports), Some(heard)) = (&mut self.reports, heard) {
            reports.ended(&heard, accepted);
        }
        let requests = completed.map_err(|failure| self.fault(failure))?;
        self.tasks(requests)
    }

    fn started(&mut self, task: workflow_graph::TaskId, attempt: u32) {
        if let Some(reports) = &mut self.reports {
            reports.started(task, attempt);
        }
    }

    fn wake_at(&self) -> Option<std::time::Instant> {
        self.calls.wake_at()
    }

    /// Asks the engine for the agent tasks whose model calls ended.
    fn wake(&mut self) -> Result<Vec<Task>, DriverError> {
        let answered = self.calls.answered();
        self.tasks(answered)
    }

    fn take_acceptance(&mut self, task: workflow_graph::TaskId, attempt: u32) -> bool {
        self.acceptance.remove(&(task, attempt)).unwrap_or(false)
    }

    fn draining(&self) -> bool {
        self.run.as_ref().is_some_and(Run::draining)
    }

    fn finish(&mut self) -> Result<(), DriverError> {
        let run = self.run.as_mut().ok_or_else(not_started)?;
        run.ready().map_err(error)?;
        // A failed check turns the run into a failed one before the `onComplete` handlers
        // see it, and `failed` then ends it.
        if let Some(reports) = &mut self.reports {
            reports.check().map_err(DriverError::new)?;
        }
        run.finish(self.settings.host.as_mut()).map_err(error)?;
        if let Some(reports) = &mut self.reports {
            reports.run_ended(None, None, run.outputs());
        }
        self.write_reports()
    }

    fn failed(&mut self, failure: &workflow_graph::ExecuteError) -> Result<(), DriverError> {
        let Some(run) = &mut self.run else {
            return Ok(());
        };
        let cancelled = matches!(failure, workflow_graph::ExecuteError::Cancelled);
        let message = match (failure, &self.cause) {
            (workflow_graph::ExecuteError::Driver(_), Some(cause)) => cause.clone(),
            _ => failure.to_string(),
        };
        let handled = run
            .failed(
                &message,
                if cancelled { 130 } else { 1 },
                matches!(failure, workflow_graph::ExecuteError::Driver(_)),
                self.settings.host.as_mut(),
            )
            .map_err(error);
        if let Some(reports) = &mut self.reports {
            reports.run_ended(Some(&message), run.failing_task(), run.outputs());
        }
        run.take_deliveries();
        let reports = self.write_reports();
        match (handled, reports) {
            (Err(handled), Err(reports)) => Err(DriverError::new(format!("{handled}; {reports}"))),
            (Err(error), _) | (_, Err(error)) => Err(error),
            _ => Ok(()),
        }
    }

    fn notes(&mut self) -> Vec<Note> {
        std::mem::take(&mut self.notes)
    }

    fn delivered(&mut self, delivery: &Delivery) -> Result<(), DriverError> {
        // The reports are uwe's own files, which the workflow does not publish.
        let written = self.folder().join("reports");
        if let Some(reports) = &mut self.reports
            && !Path::new(&delivery.location).starts_with(&written)
        {
            reports.published(delivery);
        }
        Ok(())
    }

    fn deliveries(&mut self) -> Vec<Delivery> {
        let mut deliveries = match self.run.as_mut() {
            Some(run) => run.take_deliveries(),
            None => Vec::new(),
        };
        deliveries.append(&mut self.report_deliveries);
        // The folder of the run goes with the driver, and a link into it with it.
        let folder = self.folder();
        for delivery in &mut deliveries {
            if Path::new(&delivery.location).starts_with(&folder)
                && matches!(
                    delivery.mode,
                    DeliveryMode::Link | DeliveryMode::RelativeLink
                )
            {
                delivery.mode = DeliveryMode::Copy;
            }
        }
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
        // The check reads command-line texts as the types of the schema, but Nextflow 26.04.6
        // gives scripts the params of the launch as they are, like `--n 5` as the text '5'.
        let launched = params(&self.launch, loaded.legacy)?;
        tree::deep_merge(&loaded.config.params(), &launched);
        Ok(())
    }

    fn use_files(&mut self, files: Arc<dyn Files>) {
        self.files = Some(files);
    }

    /// What the entry workflow emitted, in a real run.
    fn results(&self) -> Option<serde_json::Value> {
        let emitted = (!self.launch.simulated).then(|| self.emitted().ok().flatten());
        emitted.flatten()
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

/// Drops simulated outputs a script cannot make, preserving exact input paths and folders.
/// Optional missing files stay absent; required ones fail the task, matching Nextflow stub runs.
/// See the crate README for simulated output collection.
fn drop_simulated_outputs(request: &TaskRequest, outcome: &mut Outcome) {
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

pub(crate) fn error(error: Error) -> DriverError {
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

/// The project directory of a target: the target when it is a directory, which runs its
/// `manifest.mainScript`, else the folder of the script. A bare file name like `main.nf` is in
/// the launch directory.
fn project_folder(target: &Path) -> PathBuf {
    if target.is_dir() {
        return target.to_owned();
    }
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    parent.unwrap_or(Path::new(".")).to_owned()
}

/// The warnings about the plugins of the config of a launch that a run skips, which `uwe
/// check` lists. A config that does not load gives none; the check reports its error.
pub fn plugin_warnings(launch: &Launch, host: &mut dyn Host) -> Vec<String> {
    let project = project_folder(&launch.target);
    match load_config(&project, launch, host, false) {
        Ok((config, _)) => nextflow_plugins::catalogue::warnings(&config),
        Err(_) => Vec::new(),
    }
}

/// The config of a project directory or of a script's folder, as `nextflow run` and `nextflow
/// config` read it.
pub fn config(target: &Path, launch: &Launch, host: &mut dyn Host) -> Result<Config, DriverError> {
    load_config(&project_folder(target), launch, host, false).map(|(config, _)| config)
}

/// The config of a project directory and the files it came from, in the order read.
fn load_config(
    project: &Path,
    launch: &Launch,
    host: &mut dyn Host,
    typed: bool,
) -> Result<(Config, Vec<PathBuf>), DriverError> {
    let launch_dir = launch_folder(host);
    let project = std::path::absolute(project).unwrap_or_else(|_| project.to_owned());
    let files = nextflow_config::files(&project, &launch_dir, &launch.configs);
    let options = nextflow_config::Options {
        profiles: (!launch.profiles.is_empty()).then(|| launch.profiles.clone()),
        params: params(launch, typed)?,
        project_dir: project.to_string_lossy().into_owned(),
        launch_dir: launch_dir.to_string_lossy().into_owned(),
        work_dir: launch_dir.join("work").to_string_lossy().into_owned(),
    };
    let config = nextflow_config::load(&files, &options, host).map_err(|error| DriverError {
        unsupported: error.unsupported(),
        message: error.to_string(),
    })?;
    Ok((config, files))
}

/// The launch folder of a run, which its host gives, so that runs in one process can have
/// folders of their own.
fn launch_folder(host: &dyn Host) -> PathBuf {
    PathBuf::from(host.launch_dir())
}

/// The params of a launch as nested maps: those of the params file (`-params-file`, JSON or
/// YAML), with the command-line params over them, as text or, if `typed`, as the legacy
/// parser reads them.
pub fn params(launch: &Launch, typed: bool) -> Result<Value, DriverError> {
    let params = match &launch.inputs {
        Some(file) => params_file(file)?,
        None => tree::map(),
    };
    for (name, text) in &launch.params {
        let value = match typed {
            true => typed_param(text),
            false => Value::string(text.as_str()),
        };
        set_param(&params, name, value).map_err(DriverError::new)?;
    }
    Ok(params)
}

/// Nextflow's `parseParamValue` with `NXF_SYNTAX_PARSER=v1`: `true` and `false` in any case,
/// then whole and decimal numbers as Integer, Long or Double, else the text.
fn typed_param(text: &str) -> Value {
    match text.to_lowercase().as_str() {
        "true" => return Value::Boolean(true),
        "false" => return Value::Boolean(false),
        _ => {}
    }
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    let unsigned = text.strip_prefix('-').unwrap_or(text);
    let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, "0"));
    if !digits(whole) || !digits(fraction) {
        return Value::string(text);
    }
    if let Ok(value) = text.parse::<i32>() {
        Value::integer(value)
    } else if let Ok(value) = text.parse::<i64>() {
        Value::Number(Number::Long(value))
    } else {
        text.parse::<f64>().map_or_else(
            |_| Value::string(text),
            |value| Value::Number(Number::Double(value)),
        )
    }
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
fn set_param(params: &Value, name: &str, value: Value) -> Result<(), String> {
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
    tree::assign(params, &path, value)
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
            variables.insert(
                name.0.to_string(),
                if matches!(value, Value::Null) {
                    String::new()
                } else {
                    value.to_string()
                },
            );
        }
    }
    variables
}

/// Whether the script of a task names one of these `bin` scripts as a word of its own. As in
/// Nextflow, only such a task depends on `bin`, so a change there reruns only those.
fn names_bin_script(task: &Task, scripts: &[String]) -> bool {
    let part = |letter: char| letter.is_alphanumeric() || matches!(letter, '.' | '_' | '-');
    let named = |text: &str, name: &str| {
        text.match_indices(name).any(|(at, _)| {
            let before = text[..at].chars().next_back();
            let after = text[at + name.len()..].chars().next();
            !before.is_some_and(part) && !after.is_some_and(part)
        })
    };
    let mut texts = task
        .inputs
        .iter()
        .filter_map(|input| input.content.as_deref());
    texts.any(|text| scripts.iter().any(|name| named(text, name)))
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
    let names = crate::task::secrets(&request.directives).map_err(error)?;
    if !names.is_empty() {
        environment.insert(
            crate::task::SECRET_NAMES.into(),
            serde_json::to_string(&names).unwrap(),
        );
    }
    environment.extend(
        request
            .input_environment
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string())),
    );
    if request.made.is_some() || request.agent.is_some() {
        return Ok(exec_task(request, environment, reuse));
    }
    let mut resources = resources(request).map_err(DriverError::new)?;
    crate::options::apply(&request.directives, &mut resources, &mut environment).map_err(error)?;
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
    let launcher = launcher(request);
    for (source, name) in &request.staged {
        let (source, directory) = match source.prefix {
            None => (source.path.clone(), Path::new(&source.path).is_dir()),
            Some(_) => (source.uri(), false),
        };
        inputs.push(Input {
            executable: false,
            path: if launcher.is_some() {
                format!(".command.stage/{name}")
            } else {
                name.clone()
            },
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
    let mut command = command(request);
    if let Some(launcher) = launcher {
        inputs.push(Input {
            path: ".command.hooks".into(),
            content: Some(launcher),
            ..Default::default()
        });
        command.splice(0..0, ["/bin/bash".into(), ".command.hooks".into()]);
    }
    Ok(Task {
        output_manifest: None,
        run_as: None,
        site: None,
        // Only a `stdout` output reads the standard output, from its log.
        defer_logs: true,
        id: workflow_graph::TaskId(request.id.0),
        name: request.name.clone(),
        step: request.process.to_string(),
        image: container(request),
        command,
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

/// Reads task output files, stdout, and env/eval values from .command.env through run files.
/// Unwritten values are empty; interpreter exec tasks have no image or command and finish at once.
/// See the crate README for outcome and exec-task contracts.
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
        output_manifest: None,
        run_as: None,
        site: None,
        defer_logs: false,
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

/// The most bytes of standard output that a `stdout` output takes as text.
const STDOUT_LIMIT: u64 = 16 * 1024 * 1024;

/// The standard output of a task from its log, read through `files` up to [`STDOUT_LIMIT`].
fn stdout_text(files: Option<&dyn Files>, log: &workflow_graph::Stream) -> std::io::Result<String> {
    use std::io::Read;
    let too_large = || {
        std::io::Error::other(format!(
            "it is larger than the {} MiB that a `stdout` output takes; write it to a file \
             and use a `path` output",
            STDOUT_LIMIT >> 20
        ))
    };
    if log.size > STDOUT_LIMIT {
        return Err(too_large());
    }
    let path = match files {
        Some(files) => files.local(&log.location)?,
        None => PathBuf::from(&log.location),
    };
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(STDOUT_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > STDOUT_LIMIT {
        return Err(too_large());
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
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
    if let Some(log) = outcome
        .stdout_log
        .filter(|_| request.stdout && outcome.exit_code == 0)
    {
        match stdout_text(files, &log) {
            Ok(text) => result.stdout = text,
            Err(error) => {
                result.failure = Some(Error::Thrown {
                    exception: Rc::new(Exception::new(
                        "IOException",
                        format!("{}: cannot read the standard output: {error}", request.name),
                    )),
                    span: Default::default(),
                });
                return Ok(result);
            }
        }
    }
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
            // The file is uwe's own, unless an output asks for it.
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
