//! Evaluates Snakefiles, plans dependent jobs, and replans after checkpoint outputs arrive.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread::JoinHandle;

use python_evaluator::{Args, Error, Interpreter, Value};
use sha2::Digest;
use snakemake_parser::keyword::GlobalKeyword;
use storage::Storage;
use workflow_graph::{
    Delivery, DeliveryMode, Driver, DriverError, Files, Inputs, Language, Launch, Marker,
    MarkerKind, Note, Outcome, Task, TaskId,
};

use crate::commands::{Channel, CommandRunner, InputsReply, OutputsReply, Reply, Request};
use crate::files::{Produced, WorkflowFiles};
use crate::plan::Planner;
use crate::python::{ContainerStarter, PythonContainers};
use crate::remote::Mirrors;
use crate::workflow::{Hook, State};

/// The Snakefile of a workflow folder, as Snakemake looks for it.
pub fn find_snakefile(folder: &Path) -> Option<PathBuf> {
    [
        "Snakefile",
        "snakefile",
        "workflow/Snakefile",
        "workflow/snakefile",
    ]
    .iter()
    .map(|name| folder.join(name))
    .find(|path| path.is_file())
}

/// Turns an evaluation error into a driver error; unsupported features exit with 33.
pub fn driver_error(error: &Error, workdir: &Path) -> DriverError {
    let prefix = format!("{}/", workdir.display());
    let message = error.to_string().replace(&prefix, "");
    // A package that the workflow imports and that is not there is missing, as in Python;
    // modules of the standard library without a stand-in are unsupported.
    if error.is_unsupported() {
        DriverError::unsupported(message)
    } else {
        DriverError::new(message)
    }
}

/// A workflow run: the interpreter with the evaluated Snakefile, and the planned jobs.
pub struct Session {
    launch: Launch,
    workdir: PathBuf,
    snakefile: PathBuf,
    interpreter: Interpreter,
    state: Rc<RefCell<State>>,
    hook: Rc<Hook>,
    files: Rc<WorkflowFiles>,
    planner: Option<Planner>,
    roots: Vec<usize>,
    needrun: HashMap<usize, bool>,
    forced: HashSet<usize>,
    /// The jobs that `prioritize` of the profile names and the jobs they wait for, which
    /// start first, as Snakemake gives them the highest priority.
    urgent: HashSet<usize>,
    tasks: HashMap<usize, TaskId>,
    jobs_of_tasks: HashMap<TaskId, usize>,
    /// The tasks of service jobs with the jobs that use them, which run as one task: each
    /// job with the outputs that it declared (`services`).
    groups: HashMap<TaskId, Vec<(usize, Vec<String>)>>,
    components: HashMap<usize, Rc<Vec<usize>>>,
    done: HashSet<usize>,
    outstanding: usize,
    /// The number of the next task, which commands of workflow code share.
    next_task: Rc<Cell<u64>>,
    replan: bool,
    /// The planned jobs in the order their tasks are asked for, the position of each, and
    /// the jobs that wait for each job, as the last planning left them.
    order: Vec<usize>,
    position: HashMap<usize, usize>,
    dependents: HashMap<usize, Vec<usize>>,
    /// How many jobs that must run each job still waits for.
    remaining: HashMap<usize, usize>,
    /// The markers found since the engine last asked, like the tasks of checkpoints.
    notes: Vec<Note>,
    /// The attempt of each job whose task was asked for, from 1.
    attempts: HashMap<usize, u32>,
    signatures: HashMap<usize, serde_json::Value>,
    prepared: HashMap<usize, Task>,
    /// What the built tasks of jobs take of the global resources, until they run.
    usages: HashMap<usize, crate::scheduling::Usage>,
    /// The jobs that wait for global resources, which running jobs free when they end.
    held: Vec<usize>,
    gate: crate::scheduling::Gate,
    publications: Vec<SignaturePublication>,
    protections: Vec<Delivery>,
    /// The files that stay even when they are temporary, as Snakemake's `derived_targetfiles`:
    /// the target files, and the inputs of target jobs without outputs, like those of `all`.
    kept: Vec<String>,
    /// The outputs to deliver to the working directory, which the driver hands to the engine.
    pub deliveries: Arc<Mutex<Vec<Delivery>>>,
    /// How the evaluation of the Snakefile ended, once it ran; it runs once.
    evaluation: Option<Result<(), DriverError>>,
    /// What the code of jobs needs, like the stand-in of the snakemake package.
    embedded: crate::embedded::Setting,
    /// The folder of the `touch()` outputs that the driver made, once there is one.
    touched: Option<PathBuf>,
    /// The source files of the workflow, which every job sees in its work directory; found
    /// when the run starts.
    sources: Rc<crate::sources::Sources>,
    /// Why the jobs that failed failed, in the order they ended. After the first, no job
    /// starts unless the run keeps going, and the run fails once the running jobs have ended,
    /// as Snakemake does.
    failures: Vec<String>,
    acceptance: Arc<Mutex<HashSet<(TaskId, u32)>>>,
    hooks_started: bool,
    hooks_finished: bool,
    rerun_triggers: Option<Vec<String>>,
    /// Whether jobs run in the working directory itself, as in Snakemake, rather than each
    /// in a folder of its own; the tasks that run there, with what the session must undo.
    shared: bool,
    placed: HashMap<TaskId, crate::shared::Placed>,
    /// The CWL runs of `cwl:` jobs by job, the job of each of their tasks, the CWL documents
    /// of jobs whose run has not started, and what the runs need from the session.
    cwl: HashMap<usize, crate::cwl::Run>,
    cwl_tasks: HashMap<TaskId, usize>,
    cwl_jobs: HashMap<usize, crate::cwl::Job>,
    cwl_engine: Option<(String, executor::CancellationToken)>,
    storage: Storage,
    /// Temporary files whose removal waits until no checkpoint is pending, with their jobs.
    deferred: Vec<(String, usize)>,
    /// Whether jobs run on this machine, where Snakemake forms no group jobs.
    local: bool,
    /// The files written into the shared directory for running tasks, with how many need them.
    written: HashMap<PathBuf, usize>,
    /// The lock of the working directory while jobs run, as Snakemake takes it.
    lock: Option<crate::locks::Lock>,
    /// A name of this run among others in the folder, for the files that it holds there.
    run: String,
}

struct SignaturePublication {
    deliveries: Vec<Delivery>,
    records: Vec<(String, serde_json::Value)>,
}

impl Session {
    pub fn new(launch: &Launch, storage: &Storage) -> Result<Session, DriverError> {
        // Config files are relative to where the run starts, as Snakemake takes them.
        let mut launch = launch.clone();
        for path in launch.configs.iter_mut().chain(launch.inputs.iter_mut()) {
            *path = std::path::absolute(&*path).unwrap_or_else(|_| path.clone());
        }
        let launch = &launch;
        let target = std::path::absolute(&launch.target).unwrap_or_else(|_| launch.target.clone());
        let (snakefile, workdir) = if target.is_dir() {
            let snakefile = find_snakefile(&target).ok_or_else(|| {
                DriverError::new(format!("No Snakefile found in {}", target.display()))
            })?;
            (snakefile, target.clone())
        } else if target.is_file() {
            let workdir =
                std::env::current_dir().map_err(|error| DriverError::new(error.to_string()))?;
            (target.clone(), workdir)
        } else {
            return Err(DriverError::new(format!(
                "{} does not exist",
                target.display()
            )));
        };
        let mirrors = Rc::new(Mirrors::new(
            launch.scratch.join("downloads").join("snakemake"),
            storage,
        ));
        let files = Rc::new(WorkflowFiles::new(&workdir, mirrors.clone()));
        if !launch.simulated {
            files.write_to_disk();
        }
        let mut interpreter = Interpreter::new(files.clone());
        interpreter.stack_limit = 480 << 20;
        let globals = interpreter.new_globals("snakemake.workflow", None);
        let state = Rc::new(RefCell::new(State::new(
            globals.clone(),
            snakefile.clone(),
            workdir.clone(),
            mirrors,
        )));
        state.borrow_mut().logfile = launch.scratch.join("snakemake.log");
        crate::functions::install(&globals, &state);
        crate::utils::register(&mut interpreter, &state, &globals);
        crate::namespace::install(&mut interpreter, &globals)
            .map_err(|error| driver_error(&error, &workdir))?;
        let hook = Rc::new(Hook {
            state: state.clone(),
        });
        interpreter.hook = Some(hook.clone());
        interpreter.attribute_hook = Some(Rc::new(crate::iofile::FileAttributes));
        Ok(Session {
            launch: launch.clone(),
            workdir,
            snakefile,
            interpreter,
            state,
            hook,
            files,
            planner: None,
            roots: Vec::new(),
            needrun: HashMap::new(),
            forced: HashSet::new(),
            urgent: HashSet::new(),
            tasks: HashMap::new(),
            jobs_of_tasks: HashMap::new(),
            groups: HashMap::new(),
            components: HashMap::new(),
            done: HashSet::new(),
            outstanding: 0,
            next_task: Rc::new(Cell::new(1)),
            replan: false,
            order: Vec::new(),
            position: HashMap::new(),
            dependents: HashMap::new(),
            remaining: HashMap::new(),
            notes: Vec::new(),
            attempts: HashMap::new(),
            signatures: HashMap::new(),
            prepared: HashMap::new(),
            usages: HashMap::new(),
            held: Vec::new(),
            gate: crate::scheduling::Gate::default(),
            publications: Vec::new(),
            protections: Vec::new(),
            kept: Vec::new(),
            deliveries: Arc::default(),
            evaluation: None,
            embedded: crate::embedded::Setting::new(&launch.scratch, launch.simulated),
            touched: None,
            sources: Rc::default(),
            failures: Vec::new(),
            acceptance: Arc::default(),
            hooks_started: false,
            hooks_finished: false,
            rerun_triggers: None,
            shared: false,
            placed: HashMap::new(),
            cwl: HashMap::new(),
            cwl_tasks: HashMap::new(),
            cwl_jobs: HashMap::new(),
            cwl_engine: None,
            storage: storage.clone(),
            deferred: Vec::new(),
            local: false,
            written: HashMap::new(),
            lock: None,
            run: {
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |time| time.as_nanos());
                format!("{}-{nanos}", std::process::id())
            },
        })
    }

    /// What the session noted since the last call: task outputs that workflow code read or
    /// listed, and the tasks of checkpoints.
    fn take_notes(&mut self) -> Vec<Note> {
        let mut notes = std::mem::take(&mut self.notes);
        notes.append(&mut self.files.notes.borrow_mut());
        notes
    }

    fn error(&self, error: &Error) -> DriverError {
        let mut error = driver_error(error, &self.workdir);
        // Files on the web by their address rather than their local copy.
        error.message = self.state.borrow().mirrors.describe(&error.message);
        // A simulated run goes on without the input files that do not exist; say so, as the error
        // may come from that.
        let assumed = self
            .planner
            .as_ref()
            .map(Planner::all_assumed_files)
            .unwrap_or_default();
        if let Some(first) = assumed.first() {
            error.message.push_str(&format!(
                " (the simulated run assumed {} input files that do not exist and that no rule makes, like {first}; a real run needs them)",
                assumed.len()
            ));
        }
        // Code that reads what a task made gets empty files from the simulator.
        let read = self.files.last_output_read.borrow().clone();
        if self.launch.simulated
            && let Some(read) = read
        {
            let shown = read.strip_prefix(&self.workdir).unwrap_or(&read);
            error.message.push_str(&format!(
                " (the workflow read {}, a task output that the simulator made without its content)",
                shown.display()
            ));
        }
        error
    }

    /// Sets the config from the command line and runs the Snakefile.
    pub fn evaluate(&mut self) -> Result<(), DriverError> {
        // The profiles of the launch, else the workflow profile that Snakemake finds by itself,
        // give config files and values unless the launch does.
        let mut profile = if self.launch.profiles.is_empty() {
            match crate::profile::find(&self.workdir, &self.snakefile) {
                Some(folder) => crate::profile::Profile::load(&mut self.interpreter, &folder),
                None => Ok(crate::profile::Profile::default()),
            }
        } else {
            // A path, or the name of a workflow profile in a `profiles` folder.
            let paths: Vec<PathBuf> = self
                .launch
                .profiles
                .iter()
                .map(|name| {
                    let path = std::path::absolute(name).unwrap_or_else(|_| PathBuf::from(name));
                    if path.exists() {
                        return path;
                    }
                    crate::profile::find_named(&self.workdir, &self.snakefile, name).unwrap_or(path)
                })
                .collect();
            crate::profile::Profile::given(&mut self.interpreter, &paths)
        }
        .map_err(|error| self.error(&error))?;
        if let Some(triggers) = &self.rerun_triggers {
            crate::profile::validate_triggers(triggers).map_err(|error| self.error(&error))?;
            profile.rerun_triggers = Some(triggers.clone());
        }
        let launch_configs: Vec<PathBuf> = self
            .launch
            .configs
            .iter()
            .chain(self.launch.inputs.iter())
            .cloned()
            .collect();
        let config_paths = if launch_configs.is_empty() {
            profile.configfiles.clone()
        } else {
            launch_configs
        };
        let params = if self.launch.params.is_empty() {
            profile.config.clone()
        } else {
            self.launch.params.clone()
        };
        {
            let mut state = self.state.borrow_mut();
            for (name, value) in profile.global_resources() {
                if let ("_cores", Value::Int(cores)) = (name.as_str(), &value) {
                    state.cores = *cores;
                }
                state.global_resources.insert(name, value);
            }
            state.scatter.extend(profile.set_scatter.clone());
            state.profile = profile;
        }
        let overwrite = Value::dict(python_evaluator::Dict::default());
        let mut configfiles = Vec::new();
        for path in &config_paths {
            let text = path.to_string_lossy().into_owned();
            let loaded = crate::config::load_config(&mut self.interpreter, &text)
                .map_err(|error| self.error(&error))?;
            crate::config::update(&mut self.interpreter, &overwrite, &loaded)
                .map_err(|error| self.error(&error))?;
            configfiles.push(text);
        }
        // Each value of `--config` merges into the values of the config files, as Snakemake's
        // `parse_config` and `update_config` do: a mapping keeps the keys that it does not set.
        for (name, value) in &params {
            let name = name.trim_start_matches('-').to_owned();
            let mut entry = python_evaluator::Dict::default();
            entry.insert(&name, crate::config::parse_value(value));
            crate::config::update(&mut self.interpreter, &overwrite, &Value::dict(entry))
                .map_err(|error| self.error(&error))?;
        }
        // The config starts as a copy of the config from the command line.
        let config = Value::dict(python_evaluator::Dict::default());
        crate::config::update(&mut self.interpreter, &config, &overwrite)
            .map_err(|error| self.error(&error))?;
        {
            let mut state = self.state.borrow_mut();
            state.config = config.clone();
            state.overwrite_config = overwrite;
            state.overwrite_configfiles = configfiles;
            state.globals.set("config", config.clone());
        }
        // Path variables of the config from the command line, as the workflow starts with them.
        let pathvars = crate::pathvars::Pathvars::from_config(
            &mut self.interpreter,
            &config,
            crate::pathvars::Level::Config,
        )
        .map_err(|error| self.error(&error))?;
        self.hook
            .update_pathvars(&pathvars)
            .map_err(|error| self.error(&error))?;
        let snakefile = self.snakefile.clone();
        self.hook
            .include(&mut self.interpreter, &snakefile, true)
            .map_err(|error| self.error(&error))
    }

    /// Plans the jobs of the targets, keeping jobs that already have tasks.
    fn plan(&mut self) -> Result<(), DriverError> {
        if self.planner.is_none() {
            let state = self.state.borrow();
            self.planner = Some(Planner::new(
                state.rules.clone(),
                state.ruleorder.clone(),
                self.launch.simulated,
            ));
            if let Some(planner) = self.planner.as_mut() {
                planner.allowed = state.profile.allowed_rules.clone();
                planner.batch = state.profile.batch.clone();
            }
        }
        let mut targets: Vec<String> = if self.launch.targets.is_empty() {
            self.state.borrow().default_target.iter().cloned().collect()
        } else {
            self.launch.targets.clone()
        };
        let planner = self
            .planner
            .as_mut()
            .ok_or_else(|| DriverError::new("the Snakemake session has not planned its jobs"))?;
        planner.reset();
        let prioritize = self.state.borrow().profile.prioritize.clone();
        let [until, mut omit_from, forcerun, prioritize] = [
            &self.launch.until,
            &self.launch.omit_from,
            &self.launch.forcerun,
            &prioritize,
        ]
        .map(|targets| {
            targets
                .iter()
                .map(|target| {
                    if planner.rules.iter().any(|rule| rule.name == *target)
                        || Path::new(target).is_absolute()
                    {
                        target.clone()
                    } else {
                        crate::functions::normpath(target)
                    }
                })
                .collect::<Vec<_>>()
        });
        planner.forcerun = forcerun;
        planner.forceall = self.launch.forceall;
        // The jobs from the `batch` rule on wait for the final batch, as Snakemake omits them.
        if let Some((rule, number, total)) = &planner.batch
            && number < total
        {
            omit_from.push(rule.clone());
        }
        // Prioritized files and rules without wildcards are targets too.
        for target in &prioritize {
            let rule = planner.rules.iter().find(|rule| rule.name == *target);
            if !targets.contains(target) && rule.is_none_or(|rule| !rule.has_wildcards()) {
                targets.push(target.clone());
            }
        }
        planner.mtime = self
            .state
            .borrow()
            .profile
            .rerun_triggers
            .as_ref()
            .is_none_or(|triggers| triggers.iter().any(|trigger| trigger == "mtime"));
        planner.code = self
            .state
            .borrow()
            .profile
            .rerun_triggers
            .as_ref()
            .is_none_or(|triggers| triggers.iter().any(|trigger| trigger == "code"));
        let roots = match planner.plan_targets(&mut self.interpreter, &targets, &until, &omit_from)
        {
            Ok(roots) => roots,
            Err(error) => return Err(self.error(&error)),
        };
        self.kept = (targets.iter().chain(&until).chain(&planner.forcerun))
            .filter(|target| !planner.rules.iter().any(|rule| rule.name == **target))
            .cloned()
            .collect();
        for root in &roots {
            let job = &planner.jobs[*root];
            if job.output_files().is_empty() {
                self.kept.extend(job.input_files());
            }
        }
        self.needrun = planner.needrun(&self.interpreter, &roots);
        self.forced = planner.forced_jobs(&roots);
        self.order = planner.ordered_jobs(&roots);
        let mut pending: Vec<usize> = (self.order.iter().copied())
            .filter(|job| {
                let entry = &planner.jobs[*job];
                prioritize.contains(&planner.rules[entry.rule].name)
                    || entry
                        .output_files()
                        .iter()
                        .any(|file| prioritize.contains(file))
            })
            .collect();
        self.urgent.clear();
        while let Some(job) = pending.pop() {
            if self.needrun.get(&job).copied().unwrap_or(false) && self.urgent.insert(job) {
                pending.extend(planner.jobs[job].dependencies.iter().map(|(_, job)| *job));
            }
        }
        self.prepared.clear();
        if !self.launch.simulated {
            for job in &self.order {
                let entry = &planner.jobs[*job];
                if entry
                    .dependencies
                    .iter()
                    .any(|(_, dependency)| self.needrun.get(dependency).copied().unwrap_or(false))
                {
                    self.needrun.insert(*job, true);
                }
                if self.needrun.get(job).copied().unwrap_or(false) || self.done.contains(job) {
                    continue;
                }
                let outputs: Vec<_> = entry
                    .output
                    .values
                    .iter()
                    .filter(|value| value.tag("service").is_none())
                    .filter_map(Value::as_str)
                    .collect();
                // The incomplete records of iunctus, and those of Snakemake for outputs that
                // are there, as `--rerun-incomplete` takes them.
                if outputs.iter().any(|output| {
                    let snakemake = crate::metadata::snakemake_incomplete(&self.workdir, output);
                    let path = self.interpreter.host.resolve(output);
                    crate::metadata::incomplete_path(&self.workdir, output)
                        .try_exists()
                        .unwrap_or(true)
                        || (snakemake.is_file() && std::fs::symlink_metadata(path).is_ok())
                }) {
                    self.needrun.insert(*job, true);
                    continue;
                }
                if self
                    .state
                    .borrow()
                    .profile
                    .rerun_triggers
                    .as_ref()
                    .is_some_and(|triggers| triggers.iter().all(|trigger| trigger == "mtime"))
                {
                    continue;
                }
                if !outputs
                    .iter()
                    .any(|output| crate::metadata::signature_path(&self.workdir, output).exists())
                {
                    continue;
                }
                let state = self.state.borrow();
                let setting = crate::task::TaskSetting {
                    state: &state,
                    files: &self.files,
                    workdir: &self.workdir,
                    embedded: &self.embedded,
                    sources: &self.sources,
                };
                let id = TaskId(self.next_task.get());
                self.next_task.set(id.0 + 1);
                let crate::task::Built {
                    task,
                    signature,
                    usage,
                    cwl,
                } = crate::task::build(
                    &mut self.interpreter,
                    &setting,
                    &planner.rules[entry.rule],
                    entry,
                    id,
                    1,
                )
                .map_err(|error| driver_error(&error, &self.workdir))?;
                self.usages.insert(*job, usage);
                if outputs.iter().any(|output| {
                    crate::metadata::signature_changed(
                        &self.workdir,
                        output,
                        &signature,
                        state.profile.rerun_triggers.as_deref(),
                    )
                }) {
                    self.needrun.insert(*job, true);
                    self.signatures.insert(*job, signature);
                    self.prepared.insert(*job, task);
                    // The task of a `cwl:` job is a placeholder; its CWL run does the work.
                    if let Some(cwl) = cwl {
                        self.cwl_jobs.insert(*job, cwl);
                    }
                }
            }
        }
        // Snakemake's `check_directory_outputs`: no output of a job lies in a directory
        // output of another job. Logs and benchmarks do not count, as in Snakemake.
        let mut outputs: Vec<(PathBuf, usize)> = (self.order.iter())
            .flat_map(|job| {
                (planner.jobs[*job].output.values.iter())
                    .filter_map(|value| value.as_str())
                    .map(|file| {
                        (
                            self.interpreter.host.resolve(file.trim_end_matches('/')),
                            *job,
                        )
                    })
            })
            .collect();
        // Snakemake's `check_jobs`: no two jobs make the same file. A job planned on inputs
        // that a simulated run assumed may be one that Snakemake would not plan, since the
        // input that it lacks rules it out, so such jobs do not count.
        let mut seen: HashMap<&PathBuf, usize> = HashMap::new();
        let assumed = |job: &usize| planner.jobs[*job].assumed;
        for (path, job) in &outputs {
            match seen.get(path) {
                Some(other) if other != job && !assumed(other) && !assumed(job) => {
                    let rule = |job: &usize| planner.rules[planner.jobs[*job].rule].name.clone();
                    let file = path.strip_prefix(&self.workdir).unwrap_or(path);
                    let message = format!(
                        "AmbiguousRuleException: Rules {} and {} are ambiguous for the file {}.",
                        rule(other),
                        rule(job),
                        file.display()
                    );
                    return Err(self.error(&crate::workflow::workflow_error(message)));
                }
                _ => {
                    seen.insert(path, *job);
                }
            }
        }
        if let Some(message) = pipe_problem(planner, &self.order, &self.needrun) {
            return Err(self.error(&crate::workflow::workflow_error(message)));
        }
        // Snakemake's `check_protected_output`: any output that is there and that this user
        // may not write, other than a link, stops the run, whatever its flags.
        if !self.launch.simulated {
            for job in &self.order {
                if !self.needrun.get(job).copied().unwrap_or(false) {
                    continue;
                }
                let protected: Vec<&str> = (planner.jobs[*job].output.values.iter())
                    .filter_map(Value::as_str)
                    .filter(|output| {
                        let path = self.interpreter.host.resolve(output.trim_end_matches('/'));
                        std::fs::symlink_metadata(&path).is_ok_and(|meta| !meta.is_symlink())
                            && rustix::fs::access(&path, rustix::fs::Access::WRITE_OK).is_err()
                    })
                    .collect();
                if !protected.is_empty() {
                    let rule = &planner.rules[planner.jobs[*job].rule].name;
                    return Err(DriverError::new(format!(
                        "ProtectedOutputException in rule {rule}: Write-protected output files for rule {rule}:\n    affected files:\n        {}",
                        protected.join("\n        ")
                    )));
                }
            }
        }
        outputs.sort();
        outputs.dedup();
        // Snakemake checks this when it first builds the DAG, not when a checkpoint changed
        // it, so jobs may write into the directory output of a finished checkpoint.
        if !planner.checkpoints_done.is_empty() {
            outputs.clear();
        }
        for pair in outputs.windows(2) {
            let ((parent, parent_job), (child, child_job)) = (&pair[0], &pair[1]);
            if parent != child && child.starts_with(parent) && parent_job != child_job {
                let rule = |job: &usize| planner.rules[planner.jobs[*job].rule].name.clone();
                let message = format!(
                    "ChildIOException: File/directory is a child to another output:\n{} (rule {})\n{} (rule {})",
                    parent.display(),
                    rule(parent_job),
                    child.display(),
                    rule(child_job)
                );
                return Err(self.error(&crate::workflow::workflow_error(message)));
            }
        }
        self.components.clear();
        crate::pipes::components(
            planner,
            &self.order,
            &mut self.needrun,
            &mut self.components,
        )
        .map_err(DriverError::new)?;
        if let Some(message) = pipe_problem(planner, &self.order, &self.needrun) {
            return Err(driver_error(
                &crate::workflow::workflow_error(message),
                &self.workdir,
            ));
        }
        for job in &self.order {
            if planner.jobs[*job]
                .dependencies
                .iter()
                .any(|(file, producer)| {
                    self.needrun.get(producer).copied().unwrap_or(false)
                        && !(planner.jobs[*job].input.values.iter().any(|value| {
                            value.as_str() == Some(file) && value.tag("ancient").is_some()
                        }) && self
                            .interpreter
                            .host
                            .stat(&self.interpreter.host.resolve(file))
                            .is_some())
                })
            {
                self.needrun.insert(*job, true);
            }
        }
        // A local run of Snakemake forms no group jobs; jobs of pipes and services still
        // run together.
        let profile = self.state.borrow().profile.clone();
        self.components = match self.local {
            true => HashMap::new(),
            false => crate::groups::components(
                &mut self.interpreter,
                planner,
                &self.order,
                &self.needrun,
                &self.tasks,
                &profile,
            )
            .map_err(|error| driver_error(&error, &self.workdir))?,
        };
        crate::pipes::components(
            planner,
            &self.order,
            &mut self.needrun,
            &mut self.components,
        )
        .map_err(DriverError::new)?;
        self.position = self
            .order
            .iter()
            .enumerate()
            .map(|(position, job)| (*job, position))
            .collect();
        self.dependents.clear();
        self.remaining.clear();
        for job in &self.order {
            let mut waiting_for: Vec<usize> = planner.jobs[*job]
                .dependencies
                .iter()
                .map(|(_, dependency)| *dependency)
                .collect();
            waiting_for.sort_unstable();
            waiting_for.dedup();
            let mut count = 0;
            for dependency in waiting_for {
                self.dependents.entry(dependency).or_default().push(*job);
                if self.needrun.get(&dependency).copied().unwrap_or(false)
                    && !self.done.contains(&dependency)
                {
                    count += 1;
                }
            }
            self.remaining.insert(*job, count);
        }
        self.roots = roots;
        Ok(())
    }

    /// The tasks of the jobs that can run now.
    /// The tasks of the jobs that can run now, among all planned jobs, or only among the jobs
    /// that wait for a job that finished, so that a run of many jobs stays fast.
    fn ready_tasks(&mut self, finished: Option<usize>) -> Result<Vec<Task>, DriverError> {
        let candidates: Vec<usize> = match finished {
            None => self.order.clone(),
            Some(job) => {
                let mut waiting = self.dependents.get(&job).cloned().unwrap_or_default();
                // A job that ended freed global resources that held jobs wait for.
                waiting.extend(self.held.iter().copied());
                waiting.sort_by_key(|job| self.position.get(job).copied().unwrap_or(usize::MAX));
                waiting.dedup();
                waiting
            }
        };
        // A job that uses a service starts with the service job, so that one is a candidate too.
        let mut candidates = candidates;
        let planner = self
            .planner
            .as_ref()
            .ok_or_else(|| DriverError::new("the Snakemake session has not planned its jobs"))?;
        let services: Vec<usize> = (candidates.iter())
            .flat_map(|job| {
                planner.jobs[*job]
                    .dependencies
                    .iter()
                    .map(|(_, producer)| *producer)
            })
            .filter(|producer| !crate::services::service_files(&planner.jobs[*producer]).is_empty())
            .collect();
        for producer in services {
            if !candidates.contains(&producer) {
                candidates.push(producer);
            }
        }
        let anchors: Vec<usize> = candidates
            .iter()
            .filter_map(|job| self.components.get(job)?.first().copied())
            .collect();
        for anchor in anchors {
            if !candidates.contains(&anchor) {
                candidates.push(anchor);
            }
        }
        candidates.sort_by_key(|job| !self.urgent.contains(job));
        let mut tasks = Vec::new();
        for job in candidates {
            if !self.needrun.get(&job).copied().unwrap_or(false) || self.tasks.contains_key(&job) {
                continue;
            }
            let planner = self.planner.as_ref().ok_or_else(|| {
                DriverError::new("the Snakemake session has not planned its jobs")
            })?;
            let entry = &planner.jobs[job];
            if entry.incomplete {
                continue;
            }
            if self.remaining.get(&job).copied().unwrap_or(0) > 0 {
                continue;
            }
            let services = crate::services::service_files(entry);
            let piped = self
                .components
                .get(&job)
                .is_some_and(|jobs| !crate::pipes::files(planner, jobs).is_empty());
            if !services.is_empty() && !piped {
                tasks.extend(self.service_group(job, &services, None)?);
                continue;
            }
            if let Some(members) = self.components.get(&job).cloned()
                && members.len() > 1
            {
                if members.first() == Some(&job) {
                    tasks.extend(self.rule_group(&members, None)?);
                }
                if self.components.contains_key(&job) {
                    continue;
                }
            }
            let planner = self
                .planner
                .as_ref()
                .ok_or_else(|| DriverError::new("No Snakemake plan"))?;
            let entry = &planner.jobs[job];
            let prepared = self.prepared.remove(&job);
            let id = prepared.as_ref().map_or_else(
                || {
                    let id = TaskId(self.next_task.get());
                    self.next_task.set(id.0 + 1);
                    id
                },
                |task| task.id,
            );
            let rule = planner.rules[entry.rule].clone();
            let entry = entry.clone();
            let task = if let Some(task) = prepared {
                task
            } else {
                let crate::task::Built {
                    task,
                    signature,
                    usage,
                    cwl,
                } = {
                    let state = self.state.borrow();
                    let setting = crate::task::TaskSetting {
                        state: &state,
                        files: &self.files,
                        workdir: &self.workdir,
                        embedded: &self.embedded,
                        sources: &self.sources,
                    };
                    crate::task::build(&mut self.interpreter, &setting, &rule, &entry, id, 1)
                }
                .map_err(|error| self.error(&error))?;
                self.signatures.insert(job, signature);
                self.usages.insert(job, usage);
                if let Some(cwl) = cwl {
                    self.cwl_jobs.insert(job, cwl);
                }
                task
            };
            let usage = self.tallied(self.usages.get(&job).cloned().unwrap_or_default());
            let limits = crate::scheduling::limits(&self.state.borrow().global_resources);
            if !self.gate.fits(&usage, &limits) || self.updates_read(job) {
                self.prepared.insert(job, task);
                if !self.held.contains(&job) {
                    self.held.push(job);
                }
                continue;
            }
            self.held.retain(|held| *held != job);
            self.usages.remove(&job);
            let task = match self.share(&[job], vec![task], |members| Ok(members.remove(0)))? {
                Some(task) => task,
                None => continue,
            };
            self.gate.take(id, usage);
            if rule.checkpoint {
                let reason = format!(
                    "rule `{}` is a checkpoint: Snakemake plans the workflow again after it",
                    rule.name
                );
                let marker = Marker::new(MarkerKind::Checkpoint, reason);
                self.notes.push(Note::Mark(id, marker));
            }
            let mut task = task;
            if self.forced.contains(&job) {
                task.reuse = workflow_graph::Reuse::Denied;
            }
            // A job whose input function looked into a checkpoint's outputs runs after it,
            // even when it reads none of its files.
            let planner = self.planner.as_ref().ok_or_else(|| {
                DriverError::new("the Snakemake session has not planned its jobs")
            })?;
            for file in &entry.checkpoints {
                for (other, other_task) in &self.tasks {
                    if planner.jobs[*other].output_files().contains(file)
                        && !task.after.contains(other_task)
                    {
                        task.after.push(*other_task);
                    }
                }
            }
            self.planner
                .as_mut()
                .ok_or_else(|| DriverError::new("the Snakemake session has not planned its jobs"))?
                .fixed
                .insert(job);
            self.tasks.insert(job, id);
            self.attempts.insert(job, 1);
            self.jobs_of_tasks.insert(id, job);
            self.outstanding += 1;
            match self.cwl_jobs.remove(&job) {
                Some(cwl) => tasks.extend(self.start_cwl(job, id, cwl)?),
                None => tasks.push(task),
            }
        }
        Ok(tasks)
    }

    /// Starts the CWL run of a `cwl:` job, whose task `id` ends once the run did. Snakemake
    /// writes the input object to a file and runs cwltool, which leaves the outputs in the
    /// working directory.
    fn start_cwl(
        &mut self,
        job: usize,
        id: TaskId,
        cwl: crate::cwl::Job,
    ) -> Result<Vec<Task>, DriverError> {
        let folder = (self.launch.scratch.join("snakemake"))
            .join("cwl")
            .join(id.0.to_string());
        let inputs = folder.join("job.json");
        std::fs::create_dir_all(&folder)
            .and_then(|()| std::fs::write(&inputs, cwl.inputs.to_string()))
            .map_err(|error| DriverError::new(format!("cannot write the CWL job: {error}")))?;
        let launch = Launch {
            target: PathBuf::from(&cwl.document),
            inputs: Some(inputs),
            output_dir: Some(self.workdir.clone()),
            scratch: folder.clone(),
            simulated: self.launch.simulated,
            ..Launch::default()
        };
        let mut options = cwl_runtime::Options::default();
        if let Some(image) = self.state.borrow().default_image.clone() {
            options.image = image;
        }
        if let Some((engine, cancel)) = &self.cwl_engine {
            options.javascript =
                cwl_runtime::JavaScriptSettings::new(engine.clone(), cancel.clone());
        }
        let downloads = folder.join("downloads");
        let mut driver =
            cwl_runtime::driver_with_options(&launch, &self.storage, downloads, options)?;
        if let Some(files) = self.files.task_files() {
            driver.use_files(files);
        }
        let tasks = driver.start()?;
        let mut run = crate::cwl::Run::new(driver, id);
        let counter = self.next_task.clone();
        let next = || {
            let id = TaskId(counter.get());
            counter.set(id.0 + 1);
            id
        };
        let tasks = run.number(tasks, &next);
        for task in &tasks {
            self.cwl_tasks.insert(task.id, job);
        }
        self.outstanding += tasks.len();
        let idle = run.idle();
        self.cwl.insert(job, run);
        if idle {
            return self.end_cwl(job, None);
        }
        Ok(tasks)
    }

    /// Takes the outcome of a task of the CWL run of a job.
    fn complete_cwl(&mut self, job: usize, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        self.outstanding = self.outstanding.saturating_sub(1);
        let Some(run) = self.cwl.get_mut(&job) else {
            return Err(DriverError::new(format!("unknown task {}", outcome.id)));
        };
        let identity = (outcome.id, outcome.attempt);
        let more = match run.complete(outcome) {
            Ok((more, accepted)) => {
                if accepted {
                    let acceptance = self.acceptance.lock();
                    acceptance
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert(identity);
                }
                more
            }
            Err(error) => return self.end_cwl(job, Some(error.message)),
        };
        let counter = self.next_task.clone();
        let next = || {
            let id = TaskId(counter.get());
            counter.set(id.0 + 1);
            id
        };
        let more = run.number(more, &next);
        let idle = run.idle();
        for task in &more {
            self.cwl_tasks.insert(task.id, job);
        }
        self.outstanding += more.len();
        if !idle {
            return Ok(more);
        }
        self.end_cwl(job, None)
    }

    /// Ends the CWL run of a job: its output files go to the working directory, and the
    /// task of the job ends with those that the job declared.
    fn end_cwl(&mut self, job: usize, error: Option<String>) -> Result<Vec<Task>, DriverError> {
        let Some(mut run) = self.cwl.remove(&job) else {
            return Ok(Vec::new());
        };
        let finished = match error {
            Some(message) => Err(message),
            None => run.finish().map_err(|error| error.message),
        };
        let declared = (self.planner.as_ref())
            .map(|planner| planner.jobs[job].output_files())
            .unwrap_or_default();
        let mut outcome = Outcome {
            id: run.task,
            attempt: self.attempts.get(&job).copied().unwrap_or(1),
            ..Outcome::default()
        };
        match finished {
            Err(message) => {
                eprintln!("{message}");
                outcome.exit_code = 1;
            }
            Ok(deliveries) => {
                for delivery in deliveries {
                    let target = PathBuf::from(&delivery.target);
                    let own = (declared.iter())
                        .find(|output| self.interpreter.host.resolve(output) == target);
                    if let Some(path) = own {
                        let local = self.files.local_location(Path::new(&delivery.location));
                        outcome.files.push(workflow_graph::OutputFile {
                            path: path.clone(),
                            location: delivery.location.clone(),
                            directory: local.as_ref().is_ok_and(|local| local.is_dir()),
                            ..workflow_graph::OutputFile::default()
                        });
                    }
                    // Jobs of the shared directory read the outputs in place at once.
                    if self.shared {
                        let local = self.files.local_location(Path::new(&delivery.location));
                        local
                            .and_then(|local| crate::shared::copy_into(&local, &target))
                            .map_err(|error| {
                                DriverError::new(format!(
                                    "cannot place {}: {error}",
                                    delivery.target
                                ))
                            })?;
                    } else if own.is_none() {
                        (self.deliveries.lock())
                            .unwrap_or_else(PoisonError::into_inner)
                            .push(delivery);
                    }
                }
            }
        }
        self.complete(outcome)
    }

    /// Runs the Python packages without a stand-in in a container of the local engine
    /// (decision 19), which sees the folders of the workflow.
    pub fn use_python_containers(&mut self, settings: PythonContainers) {
        self.use_cancellable_python(settings, executor::CancellationToken::new());
    }

    pub fn use_cancellable_python(
        &mut self,
        settings: PythonContainers,
        cancel: executor::CancellationToken,
    ) {
        self.embedded.python_image = settings.image.clone();
        self.cwl_engine = Some((settings.engine.clone(), cancel.clone()));
        let mut folders = vec![self.workdir.clone()];
        if let Some(folder) = self.snakefile.parent() {
            folders.push(folder.to_path_buf());
        }
        let target = std::path::absolute(&self.launch.target).unwrap_or_default();
        if target.is_dir() {
            folders.push(target);
        }
        if let Some(project) = self.snakefile.parent().and_then(Path::parent) {
            folders.push(project.to_path_buf());
        }
        self.interpreter
            .packages
            .set_pins(crate::python::workflow_pins(&folders));
        self.interpreter
            .packages
            .set_starter(Rc::new(ContainerStarter::with_cancel(
                settings, folders, cancel,
            )));
    }

    /// Stops the Python container once no more Python code runs: every job that must run has
    /// its task, and no checkpoint job is left whose outputs make the workflow planned again.
    fn stop_unneeded_python(&self) {
        if !self.hooks_finished && !self.state.borrow().lifecycle.is_empty() {
            return;
        }
        if !self.interpreter.packages.is_running() {
            return;
        }
        let Some(planner) = &self.planner else {
            return;
        };
        let pending = self.needrun.iter().any(|(job, needed)| {
            *needed
                && (!self.tasks.contains_key(job)
                    || (!self.done.contains(job)
                        && planner.rules[planner.jobs[*job].rule].checkpoint))
        });
        if !pending && !self.replan {
            self.interpreter.packages.stop();
        }
    }

    /// Evaluates the Snakefile, once: the inputs and the start of a run share it.
    fn evaluate_once(&mut self) -> Result<(), DriverError> {
        if let Some(result) = &self.evaluation {
            return result.clone();
        }
        let result = self.evaluate();
        self.evaluation = Some(result.clone());
        result
    }

    /// The inputs of the workflow, described once its Snakefile is evaluated.
    pub fn inputs(&mut self) -> Result<Inputs, DriverError> {
        self.evaluate_once()?;
        let workflow = crate::inputs::Workflow::of(&self.state.borrow());
        crate::inputs::inputs(&mut self.interpreter, &workflow).map_err(|error| self.error(&error))
    }

    /// The outputs of the workflow: the files of its target rules for the config of the launch.
    pub fn outputs(&mut self) -> Result<serde_json::Value, DriverError> {
        self.evaluate_once()?;
        let (rules, targets, title) = {
            let state = self.state.borrow();
            let targets: Vec<String> = match self.launch.targets.is_empty() {
                true => state.default_target.iter().cloned().collect(),
                false => self.launch.targets.clone(),
            };
            let folder = state.main_snakefile.parent();
            let title = folder.and_then(crate::inputs::workflow_folder);
            (state.rules.clone(), targets, title.unwrap_or_default())
        };
        crate::output_schema::schema(&mut self.interpreter, &rules, &targets, &title)
            .map_err(|error| self.error(&error))
    }

    pub fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        self.evaluate_once()?;
        self.sources = Rc::new(crate::sources::Sources::find(&self.state.borrow()));
        self.plan()?;
        self.report_assumed();
        self.take_lock()?;
        if self.needrun.values().any(|needed| *needed) {
            if !self.state.borrow().lifecycle.is_empty() {
                std::fs::create_dir_all(&self.launch.scratch)
                    .and_then(|()| {
                        std::fs::write(
                            self.launch.scratch.join("snakemake.log"),
                            self.files.messages.borrow().join("\n"),
                        )
                    })
                    .map_err(|error| DriverError::new(error.to_string()))?;
            }
            self.run_hook(GlobalKeyword::OnStart)?;
            self.hooks_started = true;
        }
        self.ready_tasks(None)
    }

    /// Locks the working directory for the files of the planned jobs, as Snakemake does
    /// before it runs them: the inputs of all jobs, and the outputs of those that run.
    fn take_lock(&mut self) -> Result<(), DriverError> {
        let Some(planner) = &self.planner else {
            return Ok(());
        };
        if self.launch.simulated || self.state.borrow().profile.nolock {
            return Ok(());
        }
        let mut inputs = Vec::new();
        let mut outputs = Vec::new();
        for job in &self.order {
            inputs.extend(planner.jobs[*job].input_files());
            if self.needrun.get(job).copied().unwrap_or(false) {
                let values = planner.jobs[*job].output.values.iter();
                outputs.extend(values.filter_map(Value::as_str).map(str::to_owned));
            }
        }
        for files in [&mut inputs, &mut outputs] {
            let mut seen = HashSet::new();
            files.retain(|file| seen.insert(file.clone()));
        }
        let lock = crate::locks::Lock::take(&self.workdir, &inputs, &outputs);
        self.lock = Some(lock.map_err(DriverError::new)?);
        Ok(())
    }

    fn run_hook(&mut self, keyword: GlobalKeyword) -> Result<(), DriverError> {
        let function = self.state.borrow().lifecycle.get(&keyword).cloned();
        let Some(function) = function else {
            return Ok(());
        };
        let log = self.launch.scratch.join("snakemake.log");
        let before = self.files.messages.borrow().len();
        self.files.lifecycle.set(Some(keyword.text()));
        let result = self.interpreter.call(
            &function,
            Args::new(vec![Value::str(log.to_string_lossy().as_ref())]),
        );
        self.files.lifecycle.set(None);
        let messages = self.files.messages.borrow()[before..].to_vec();
        for message in &messages {
            eprintln!("{message}");
        }
        std::fs::OpenOptions::new()
            .append(true)
            .open(&log)
            .and_then(|mut file| writeln!(file, "{}", messages.join("\n")))
            .map_err(|error| DriverError::new(error.to_string()))?;
        result.map(|_| ()).map_err(|error| self.error(&error))
    }

    fn failure_hook(&mut self, error: &str) -> Result<(), DriverError> {
        let result = if self.hooks_started && !self.hooks_finished {
            self.hooks_finished = true;
            if self
                .state
                .borrow()
                .lifecycle
                .contains_key(&GlobalKeyword::OnError)
            {
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(self.launch.scratch.join("snakemake.log"))
                    .and_then(|mut file| writeln!(file, "{error}"))
                    .map_err(|error| DriverError::new(error.to_string()))?;
            }
            self.run_hook(GlobalKeyword::OnError)
        } else {
            Ok(())
        };
        self.interpreter.packages.stop();
        result
    }

    fn report_assumed(&self) {
        let Some(planner) = &self.planner else { return };
        let assumed = planner.assumed_files(&self.roots);
        if assumed.is_empty() || std::env::var_os("IUNCTUS_QUIET").is_some() {
            return;
        }
        eprintln!(
            "snakemake: {} input files that no rule makes do not exist and are assumed to, like {}",
            assumed.len(),
            assumed[0]
        );
    }

    pub fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        if let Some(job) = self.cwl_tasks.remove(&outcome.id) {
            return self.complete_cwl(job, outcome);
        }
        let mut outcome = outcome;
        if let Some(placed) = self.placed.remove(&outcome.id) {
            let succeeded = outcome.exit_code == 0;
            let written = placed.written.clone();
            crate::shared::finish(placed, &mut outcome, succeeded).map_err(|error| {
                DriverError::new(format!("cannot finish task {}: {error}", outcome.id))
            })?;
            self.release_written(&written);
            if outcome.cached && succeeded {
                self.restore(&outcome)?;
            }
        }
        if let Some(members) = self.groups.remove(&outcome.id) {
            return self.complete_group(members, outcome);
        }
        let identity = (outcome.id, outcome.attempt);
        let Some(job) = self.jobs_of_tasks.get(&outcome.id).copied() else {
            return Err(DriverError::new(format!("unknown task {}", outcome.id)));
        };
        self.outstanding = self.outstanding.saturating_sub(1);
        // A retry keeps what the job took of the global resources.
        let freed = self.gate.release(outcome.id).unwrap_or_default();
        // Only reads while this outcome is handled explain an error of it.
        *self.files.last_output_read.borrow_mut() = None;
        if outcome.exit_code != 0
            && let Some(task) = self.retry(job, outcome.id)?
        {
            self.gate.take(task.id, freed);
            return Ok(vec![task]);
        }
        if outcome.exit_code != 0 {
            let message = format!(
                "Error in rule {}: the task ended with exit code {}",
                self.rule_name(job),
                outcome.exit_code
            );
            self.remove_failed(&[job])?;
            return self.failed(message);
        }
        if !self.launch.simulated
            && let Err(error) = self.check_outputs(job, &mut outcome)
        {
            if let Some(task) = self.retry(job, outcome.id)? {
                self.gate.take(task.id, freed);
                return Ok(vec![task]);
            }
            self.remove_failed(&[job])?;
            return self.failed(error.message);
        }
        self.drop_backups(&[job])?;
        let delivery_start = self
            .deliveries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        if let Some(message) = self.take_outcome(job, outcome)? {
            return self.failed(message);
        }
        self.queue_signatures(&[job], delivery_start)?;
        self.release_temporary(&[job])?;
        let next = self.next_tasks(&[job]);
        if next.is_ok() {
            self.accept(identity);
        }
        next
    }

    /// Takes the outcome of a group of a service job and the jobs that use it: each job gets
    /// the files that it declared.
    fn complete_group(
        &mut self,
        members: Vec<(usize, Vec<String>)>,
        outcome: Outcome,
    ) -> Result<Vec<Task>, DriverError> {
        let identity = (outcome.id, outcome.attempt);
        self.outstanding = self.outstanding.saturating_sub(1);
        self.gate.release(outcome.id);
        *self.files.last_output_read.borrow_mut() = None;
        if outcome.exit_code != 0 {
            if let Some(task) = self.retry_group(&members, outcome.id)? {
                return Ok(vec![task]);
            }
            let rules: Vec<String> = (members.iter())
                .map(|(job, _)| self.rule_name(*job))
                .collect();
            let message = format!(
                "Error in group of rules {}: the task ended with exit code {}",
                rules.join(", "),
                outcome.exit_code
            );
            let jobs: Vec<usize> = members.iter().map(|(job, _)| *job).collect();
            self.remove_failed(&jobs)?;
            return self.failed(message);
        }
        let jobs: Vec<usize> = members.iter().map(|(job, _)| *job).collect();
        let mut outcomes = Vec::new();
        for (job, declared) in &members {
            let mut own = outcome.clone();
            let trimmed = |path: &str| {
                path.trim_start_matches('/')
                    .trim_end_matches('/')
                    .to_owned()
            };
            own.files.retain(|file| {
                let path = trimmed(&file.path);
                (declared.iter()).any(|output| {
                    let output = trimmed(output);
                    path == output || path.starts_with(&format!("{output}/"))
                })
            });
            if !self.launch.simulated
                && let Err(error) = self.check_outputs(*job, &mut own)
            {
                if let Some(task) = self.retry_group(&members, outcome.id)? {
                    return Ok(vec![task]);
                }
                let jobs: Vec<usize> = members.iter().map(|(job, _)| *job).collect();
                self.remove_failed(&jobs)?;
                return self.failed(error.message);
            }
            outcomes.push((*job, own));
        }
        self.drop_backups(&jobs)?;
        let delivery_start = self
            .deliveries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        for (job, own) in outcomes {
            if let Some(message) = self.take_outcome(job, own)? {
                return self.failed(message);
            }
        }
        self.queue_signatures(&jobs, delivery_start)?;
        self.release_temporary(&jobs)?;
        let next = self.next_tasks(&jobs);
        if next.is_ok() {
            self.accept(identity);
        }
        next
    }

    /// Readies the task of `jobs` for the shared working directory when jobs run there: each
    /// job is prepared, then `combine` makes the task of their tasks. `None` when a job lacks
    /// an input, which fails it, as Snakemake's `wait_for_files` does.
    fn share(
        &mut self,
        jobs: &[usize],
        mut members: Vec<Task>,
        combine: impl FnOnce(&mut Vec<Task>) -> Result<Task, DriverError>,
    ) -> Result<Option<Task>, DriverError> {
        if !self.shared {
            return combine(&mut members).map(Some);
        }
        let planner = self
            .planner
            .as_ref()
            .ok_or_else(|| DriverError::new("the Snakemake session has not planned its jobs"))?;
        // The jobs of one task make some inputs of the others while it runs.
        let later: Vec<String> = (jobs.iter())
            .flat_map(|job| planner.jobs[*job].output_files())
            .collect();
        for job in jobs {
            let missing = crate::shared::missing_inputs(&self.workdir, &planner.jobs[*job], &later);
            if !missing.is_empty() {
                let message = format!(
                    "WorkflowError in rule {}: Missing files after the jobs before it ended: {}",
                    planner.rules[planner.jobs[*job].rule].name,
                    missing.join(", ")
                );
                self.failures.push(message);
                return Ok(None);
            }
        }
        let mut placed = crate::shared::Placed::default();
        for (job, task) in jobs.iter().zip(&mut members) {
            let entry = &planner.jobs[*job];
            let shadow = planner.rules[entry.rule].shadow.as_deref();
            crate::shared::prepare(&self.workdir, entry, shadow, task, &mut placed)
                .map_err(|error| DriverError::new(format!("cannot prepare a job: {error}")))?;
            // Outputs in place are incomplete until the job ended well, so that a run that
            // stops meanwhile makes them again, as Snakemake's incomplete markers do.
            let outputs = (entry.output.values.iter())
                .filter(|value| value.tag("service").is_none())
                .filter_map(Value::as_str);
            for output in outputs {
                crate::metadata::mark_incomplete(&self.workdir, output).map_err(|error| {
                    DriverError::new(format!("cannot mark output {output} incomplete: {error}"))
                })?;
            }
        }
        let mut task = combine(&mut members)?;
        crate::shared::place(&self.workdir, &mut task, &mut placed, &self.run)
            .map_err(|error| DriverError::new(format!("cannot place a job: {error}")))?;
        for file in &placed.written {
            *self.written.entry(file.clone()).or_default() += 1;
        }
        self.placed.insert(task.id, placed);
        Ok(Some(task))
    }

    /// Removes the files written for a task that ended once no running task needs them.
    fn release_written(&mut self, files: &[PathBuf]) {
        for file in files {
            let Some(count) = self.written.get_mut(file) else {
                continue;
            };
            *count -= 1;
            if *count == 0 {
                self.written.remove(file);
                let _ = crate::shared::release(&self.workdir, file, &self.run);
            }
        }
    }

    /// Puts the outputs of a stored result that answered a job of the shared directory into
    /// the directory, where the job would have written them; Snakemake's checks then see a
    /// job that ran.
    fn restore(&self, outcome: &Outcome) -> Result<(), DriverError> {
        for file in &outcome.files {
            let target = self.interpreter.host.resolve(&file.path);
            let restored = self
                .files
                .local_location(Path::new(&file.location))
                .and_then(|local| {
                    crate::shared::remove(&target)?;
                    crate::shared::copy_into(&local, &target)?;
                    // The store keeps its files read-only; the job's own outputs are not.
                    crate::shared::make_writable(&target)
                });
            restored.map_err(|error| {
                DriverError::new(format!("cannot restore output {}: {error}", file.path))
            })?;
        }
        Ok(())
    }

    /// Whether a job updates a file in the shared directory that another job still has to
    /// read as it was before, which Snakemake runs first (`handle_update_flags`).
    fn updates_read(&self, job: usize) -> bool {
        let Some(planner) = self.planner.as_ref().filter(|_| self.shared) else {
            return false;
        };
        let updated: Vec<&str> = (planner.jobs[job].output.values.iter())
            .filter(|value| value.tag("update").is_some())
            .filter_map(Value::as_str)
            .collect();
        !updated.is_empty()
            && self.order.iter().any(|reader| {
                *reader != job
                    && self.needrun.get(reader).copied().unwrap_or(false)
                    && !self.done.contains(reader)
                    && (planner.jobs[*reader].input.values.iter()).any(|value| {
                        value.tag("before_update").is_some()
                            && value.as_str().is_some_and(|file| updated.contains(&file))
                    })
            })
    }

    /// Whether a task of several jobs fits beside the running ones; a job of it waits for the
    /// global resources otherwise, as Snakemake's scheduler holds a group job back. A retry
    /// keeps what its task took.
    fn admit(&mut self, anchor: usize, retry: bool, usage: &crate::scheduling::Usage) -> bool {
        let limits = crate::scheduling::limits(&self.state.borrow().global_resources);
        if !retry && !self.gate.fits(&self.tallied(usage.clone()), &limits) {
            if !self.held.contains(&anchor) {
                self.held.push(anchor);
            }
            return false;
        }
        self.held.retain(|held| *held != anchor);
        true
    }

    /// What of a job's usage the run tallies: a run that submits jobs elsewhere leaves out the
    /// resources whose scope is `local`, which each job has a pool of, as in Snakemake.
    fn tallied(&self, mut usage: crate::scheduling::Usage) -> crate::scheduling::Usage {
        if !self.local {
            let state = self.state.borrow();
            usage.retain(|(name, _)| {
                state
                    .resource_scopes
                    .get(name)
                    .is_none_or(|scope| scope != "local")
            });
        }
        usage
    }

    /// Snakemake's `cleanup_workdir`: removes the empty folders of the files of the jobs that
    /// are not there, like those of temporary files, and their empty parents.
    fn clean_folders(&self) {
        let Some(planner) = &self.planner else {
            return;
        };
        let mut folders = std::collections::BTreeSet::new();
        for job in &self.order {
            let entry = &planner.jobs[*job];
            let files = (entry.output.values.iter()).chain(&entry.input.values);
            for file in files.filter_map(Value::as_str) {
                let path = self.workdir.join(file);
                if !path.exists()
                    && let Some(parent) = path.parent()
                {
                    folders.insert(parent.to_path_buf());
                }
            }
        }
        for folder in folders {
            let mut current = folder.as_path();
            // `remove_dir` removes only empty folders; the working directory stays.
            while current.starts_with(&self.workdir)
                && current != self.workdir
                && std::fs::remove_dir(current).is_ok()
            {
                let Some(parent) = current.parent() else {
                    break;
                };
                current = parent;
            }
        }
    }

    /// Removes the outputs of failed jobs in the shared directory, as Snakemake does since
    /// they might be corrupted.
    fn remove_failed(&self, jobs: &[usize]) -> Result<(), DriverError> {
        self.restore_updates(jobs)?;
        let Some(planner) = self.planner.as_ref().filter(|_| self.shared) else {
            return Ok(());
        };
        for job in jobs {
            crate::shared::remove_outputs(&self.workdir, &planner.jobs[*job], false)
                .map_err(|error| DriverError::new(format!("cannot remove outputs: {error}")))?;
        }
        Ok(())
    }

    /// Gives the `update()` outputs of failed jobs in the shared directory their content from
    /// before the jobs, as Snakemake does before it removes the other outputs or retries.
    fn restore_updates(&self, jobs: &[usize]) -> Result<(), DriverError> {
        let Some(planner) = self.planner.as_ref().filter(|_| self.shared) else {
            return Ok(());
        };
        for job in jobs {
            crate::shared::restore_updates(&self.workdir, &planner.jobs[*job])
                .map_err(|error| DriverError::new(format!("cannot restore outputs: {error}")))?;
        }
        Ok(())
    }

    /// Removes the backups of the `update()` outputs of jobs that ended well.
    fn drop_backups(&self, jobs: &[usize]) -> Result<(), DriverError> {
        let Some(planner) = self.planner.as_ref().filter(|_| self.shared) else {
            return Ok(());
        };
        for job in jobs {
            crate::shared::drop_backups(&self.workdir, &planner.jobs[*job])
                .map_err(|error| DriverError::new(format!("cannot remove a backup: {error}")))?;
        }
        Ok(())
    }

    /// Removes the `temp()` outputs of the shared directory that no job waits for anymore,
    /// as Snakemake's `handle_temp`: those of the `finished` jobs and of their inputs.
    fn release_temporary(&mut self, finished: &[usize]) -> Result<(), DriverError> {
        let notemp = self.state.borrow().profile.notemp;
        let Some(planner) = self.planner.as_ref().filter(|_| self.shared && !notemp) else {
            return Ok(());
        };
        let mut candidates = std::mem::take(&mut self.deferred);
        for job in finished {
            let own = planner.jobs[*job].temporary_files();
            candidates.extend(own.into_iter().map(|file| (file, *job)));
            for (file, producer) in &planner.jobs[*job].dependencies {
                if planner.jobs[*producer].temporary_files().contains(file) {
                    candidates.push((file.clone(), *producer));
                }
            }
        }
        // A job that waits for a checkpoint may still need any file.
        if self.replan || self.order.iter().any(|job| planner.jobs[*job].incomplete) {
            self.deferred = candidates;
            return Ok(());
        }
        for (file, producer) in candidates {
            let waiting = self.order.iter().any(|consumer| {
                self.needrun.get(consumer).copied().unwrap_or(false)
                    && !self.done.contains(consumer)
                    && (planner.jobs[*consumer].dependencies.iter())
                        .any(|(input, maker)| *input == file && *maker == producer)
            });
            if waiting || self.kept.contains(&file) {
                continue;
            }
            crate::shared::remove(&self.workdir.join(&file)).map_err(|error| {
                DriverError::new(format!("cannot remove temporary output {file}: {error}"))
            })?;
        }
        Ok(())
    }

    fn accept(&self, identity: (TaskId, u32)) {
        let mut acceptance = self
            .acceptance
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        acceptance.insert(identity);
    }

    fn queue_signatures(
        &mut self,
        jobs: &[usize],
        delivery_start: usize,
    ) -> Result<(), DriverError> {
        if self.launch.simulated {
            return Ok(());
        }
        let deliveries = self
            .deliveries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)[delivery_start..]
            .to_vec();
        if deliveries.is_empty() && !self.shared {
            return Ok(());
        }
        let planner = self
            .planner
            .as_ref()
            .ok_or_else(|| DriverError::new("the Snakemake session has not planned its jobs"))?;
        let mut records = Vec::new();
        for job in jobs {
            if let Some(signature) = self.signatures.get(job) {
                for value in &planner.jobs[*job].output.values {
                    if value.tag("service").is_none()
                        && let Some(output) = value.as_str()
                    {
                        records.push((output.to_owned(), signature.clone()));
                    }
                }
            }
        }
        // In the shared directory the outputs are in place already.
        if self.shared {
            for (output, signature) in &records {
                crate::metadata::publish_signature(&self.workdir, output, signature).map_err(
                    |error| {
                        DriverError::new(format!(
                            "cannot record successful output {output}: {error}"
                        ))
                    },
                )?;
                let _ = crate::metadata::clear_incomplete(&self.workdir, output);
            }
            return Ok(());
        }
        for (output, _) in &records {
            crate::metadata::mark_incomplete(&self.workdir, output).map_err(|error| {
                DriverError::new(format!("cannot mark output {output} incomplete: {error}"))
            })?;
        }
        self.publications.push(SignaturePublication {
            deliveries,
            records,
        });
        Ok(())
    }

    fn delivered(&mut self, delivery: &Delivery) -> Result<(), DriverError> {
        if let Some(position) = self
            .protections
            .iter()
            .position(|expected| expected == delivery)
        {
            crate::metadata::protect(Path::new(&delivery.target)).map_err(|error| {
                DriverError::new(format!(
                    "cannot protect output {}: {error}",
                    delivery.target
                ))
            })?;
            self.protections.remove(position);
        }
        for index in (0..self.publications.len()).rev() {
            let pending = &mut self.publications[index];
            if let Some(position) = pending
                .deliveries
                .iter()
                .position(|expected| expected == delivery)
            {
                pending.deliveries.remove(position);
                if pending.deliveries.is_empty() {
                    let pending = self.publications.remove(index);
                    for (output, signature) in &pending.records {
                        crate::metadata::publish_signature(&self.workdir, output, signature)
                            .map_err(|error| {
                                DriverError::new(format!(
                                    "cannot record successful output {output}: {error}"
                                ))
                            })?;
                    }
                    for (output, _) in pending.records {
                        crate::metadata::clear_incomplete(&self.workdir, &output).map_err(
                            |error| {
                                DriverError::new(format!(
                                    "cannot complete output {output} metadata: {error}"
                                ))
                            },
                        )?;
                    }
                }
                break;
            }
        }
        Ok(())
    }

    /// Takes the outcome of a job that ended well: checks its outputs, hands them to the
    /// working directory and lets the jobs after it go on. Gives why its outputs are wrong, if
    /// they are.
    fn take_outcome(
        &mut self,
        job: usize,
        outcome: Outcome,
    ) -> Result<Option<String>, DriverError> {
        // The outputs go to the working directory, as Snakemake leaves them there, except the
        // temporary ones.
        let mut temporary = (self.planner.as_ref())
            .map(|planner| planner.jobs[job].temporary_files())
            .unwrap_or_default();
        temporary.retain(|file| !self.kept.contains(file));
        // `notemp` keeps them, as Snakemake skips `handle_temp`.
        if self.state.borrow().profile.notemp {
            temporary.clear();
        }
        // An executor may give an absolute output without its leading `/`, as the simulator does.
        let declared = self
            .planner
            .as_ref()
            .map(|planner| planner.jobs[job].output_files())
            .unwrap_or_default();
        for file in &outcome.files {
            let relative = file.path.trim_end_matches('/');
            let absolute = declared.iter().find(|output| {
                output.starts_with('/')
                    && Path::new(&file.path).is_relative()
                    && output.trim_start_matches('/').trim_end_matches('/') == relative
            });
            let path = match absolute {
                Some(output) => PathBuf::from(output.trim_end_matches('/')),
                None => self.interpreter.host.resolve(&file.path),
            };
            let storage_target = self.planner.as_ref().and_then(|planner| {
                planner.jobs[job].output.values.iter().find_map(|value| {
                    let output = value.as_str()?;
                    (self.interpreter.host.resolve(output.trim_end_matches('/')) == path)
                        .then(|| value.tag(crate::storage::TAG)?.as_str().map(str::to_owned))
                        .flatten()
                })
            });
            let protected = || {
                self.planner.as_ref().is_some_and(|planner| {
                    planner.jobs[job].output.values.iter().any(|value| {
                        value.tag("protected").is_some()
                            && value.as_str().is_some_and(|output| {
                                self.interpreter.host.resolve(output.trim_end_matches('/')) == path
                            })
                    })
                })
            };
            // Snakemake protects the local copy of a file in storage, not the object.
            let stored = storage_target.is_some();
            if self.shared && stored && protected() && path.exists() {
                crate::metadata::protect(&path).map_err(|error| {
                    DriverError::new(format!("cannot protect output {}: {error}", path.display()))
                })?;
            }
            // A job of the shared directory left its outputs in place.
            if self.shared && !stored {
                if protected() {
                    crate::metadata::protect(&path).map_err(|error| {
                        DriverError::new(format!(
                            "cannot protect output {}: {error}",
                            path.display()
                        ))
                    })?;
                }
            } else if !temporary
                .iter()
                .any(|output| self.interpreter.host.resolve(output) == path)
            {
                let mut deliveries = self
                    .deliveries
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                let delivery = Delivery {
                    location: file.location.clone(),
                    target: storage_target.unwrap_or_else(|| path.to_string_lossy().into_owned()),
                    mode: DeliveryMode::Copy,
                };
                if !self.launch.simulated && !stored && protected() {
                    self.protections.push(delivery.clone());
                }
                deliveries.push(delivery);
            }
            if file.directory
                && let Some(planner) = self.planner.as_mut()
            {
                planner.made_folders.push(path.clone());
            }
            if !self.shared {
                self.files.add_output(
                    &path,
                    Produced {
                        location: PathBuf::from(&file.location),
                        directory: file.directory,
                    },
                );
            }
        }
        if !self.launch.simulated {
            self.empty_logs(job, &outcome)?;
            self.benchmark(job, &outcome)?;
        }
        self.done.insert(job);
        if self.needrun.get(&job).copied().unwrap_or(false) {
            for dependent in self.dependents.get(&job).cloned().unwrap_or_default() {
                if let Some(count) = self.remaining.get_mut(&dependent) {
                    *count = count.saturating_sub(1);
                }
            }
        }
        let planner = self
            .planner
            .as_mut()
            .ok_or_else(|| DriverError::new("the Snakemake session has not planned its jobs"))?;
        if planner.rules[planner.jobs[job].rule].checkpoint {
            planner
                .checkpoints_done
                .extend(planner.jobs[job].output_files());
            self.replan = true;
        }
        Ok(None)
    }

    /// The tasks that can run after `jobs` ended.
    fn next_tasks(&mut self, jobs: &[usize]) -> Result<Vec<Task>, DriverError> {
        // After a failure, running jobs end, but no job starts unless the run keeps going.
        if !self.failures.is_empty() && !self.launch.keep_going {
            return Ok(Vec::new());
        }
        if self.replan && self.outstanding == 0 {
            self.replan = false;
            self.plan()?;
            return self.ready_tasks(None);
        }
        let mut tasks = Vec::new();
        for job in jobs {
            tasks.extend(self.ready_tasks(Some(*job))?);
        }
        Ok(tasks)
    }

    /// The task of a service job that is ready to run, with the jobs that use its `services`
    /// outputs, once they wait for nothing else; `None` while one of them still waits.
    fn service_group(
        &mut self,
        job: usize,
        services: &[String],
        retry: Option<TaskId>,
    ) -> Result<Option<Task>, DriverError> {
        let planner = self
            .planner
            .as_ref()
            .ok_or_else(|| DriverError::new("the Snakemake session has not planned its jobs"))?;
        let mut users: Vec<usize> = (self.order.iter().copied())
            .filter(|user| {
                (planner.jobs[*user].dependencies.iter())
                    .any(|(file, producer)| *producer == job && services.contains(file))
            })
            .filter(|user| self.needrun.get(user).copied().unwrap_or(false))
            .collect();
        let direct_users = users.clone();
        let mut expanded = false;
        for user in direct_users {
            if let Some(component) = self.components.get(&user) {
                for member in component.iter() {
                    if !users.contains(member) {
                        users.push(*member);
                        expanded = true;
                    }
                }
            }
        }
        let mut connected = HashSet::from([job]);
        for member in &self.order {
            if planner.jobs[*member]
                .dependencies
                .iter()
                .any(|(_, producer)| connected.contains(producer))
            {
                connected.insert(*member);
            }
        }
        let mut required = HashSet::new();
        let mut pending = users;
        while let Some(member) = pending.pop() {
            if required.insert(member) {
                pending.extend(
                    planner.jobs[member]
                        .dependencies
                        .iter()
                        .map(|(_, producer)| *producer),
                );
            }
        }
        let users: Vec<usize> = self
            .order
            .iter()
            .copied()
            .filter(|member| {
                *member != job
                    && connected.contains(member)
                    && required.contains(member)
                    && self.needrun.get(member).copied().unwrap_or(false)
                    && !self.done.contains(member)
            })
            .collect();
        let waiting = |user: &usize| {
            planner.jobs[*user].incomplete
                || planner.jobs[*user]
                    .dependencies
                    .iter()
                    .any(|(_, producer)| {
                        *producer != job
                            && !users.contains(producer)
                            && self.needrun.get(producer).copied().unwrap_or(false)
                            && !self.done.contains(producer)
                    })
                || self.tasks.contains_key(user)
        };
        if retry.is_none() && users.iter().any(waiting) {
            return Ok(None);
        }
        let id = retry.unwrap_or_else(|| {
            let id = TaskId(self.next_task.get());
            self.next_task.set(id.0 + 1);
            id
        });
        let attempt = if retry.is_some() {
            self.attempts.get(&job).copied().unwrap_or(1) + 1
        } else {
            1
        };
        let mut members = Vec::new();
        let mut declared = Vec::new();
        let mut usages = Vec::new();
        for member in std::iter::once(job).chain(users) {
            let planner = self.planner.as_ref().ok_or_else(|| {
                DriverError::new("the Snakemake session has not planned its jobs")
            })?;
            let rule = planner.rules[planner.jobs[member].rule].clone();
            let entry = planner.jobs[member].clone();
            let crate::task::Built {
                task,
                signature,
                cwl,
                usage,
            } = {
                let state = self.state.borrow();
                let setting = crate::task::TaskSetting {
                    state: &state,
                    files: &self.files,
                    workdir: &self.workdir,
                    embedded: &self.embedded,
                    sources: &self.sources,
                };
                crate::task::build(&mut self.interpreter, &setting, &rule, &entry, id, attempt)
            }
            .map_err(|error| self.error(&error))?;
            if cwl.is_some() {
                return Err(cwl_grouped());
            }
            self.signatures.insert(member, signature);
            declared.push((member, task.outputs.clone()));
            members.push(task);
            usages.push(usage);
        }
        if expanded
            && members
                .iter()
                .any(|member| member.image != members[0].image)
        {
            tracing::debug!(
                "Snakemake service group has different images; running explicit group members separately"
            );
            for (member, _) in &declared {
                self.components.remove(member);
            }
            return self.service_group(job, services, retry);
        }
        let jobs: Vec<usize> = declared.iter().map(|(member, _)| *member).collect();
        // The jobs of a service run at once.
        let usage = crate::scheduling::combined(&usages, &vec![0; usages.len()]);
        if !self.admit(job, retry.is_some(), &usage) {
            return Ok(None);
        }
        let combine = |members: &mut Vec<Task>| {
            crate::services::combine(members, services, id).map_err(DriverError::new)
        };
        let Some(mut task) = self.share(&jobs, members, combine)? else {
            return Ok(None);
        };
        let usage = self.tallied(usage);
        self.gate.take(id, usage);
        if retry.is_some()
            || declared
                .iter()
                .any(|(member, _)| self.forced.contains(member))
        {
            task.reuse = workflow_graph::Reuse::Denied;
        }
        for (member, _) in &declared {
            self.planner
                .as_mut()
                .ok_or_else(|| DriverError::new("the Snakemake session has not planned its jobs"))?
                .fixed
                .insert(*member);
            self.tasks.insert(*member, id);
            self.attempts.insert(*member, attempt);
        }
        self.jobs_of_tasks.insert(id, job);
        self.groups.insert(id, declared);
        self.outstanding += 1;
        Ok(Some(task))
    }

    fn retry_group(
        &mut self,
        members: &[(usize, Vec<String>)],
        id: TaskId,
    ) -> Result<Option<Task>, DriverError> {
        let Some((job, _)) = members.first() else {
            return Ok(None);
        };
        let planner = self
            .planner
            .as_ref()
            .ok_or_else(|| DriverError::new("the Snakemake session has not planned its jobs"))?;
        let retries = members
            .iter()
            .filter_map(|(member, _)| {
                planner.rules[planner.jobs[*member].rule]
                    .retries
                    .as_ref()
                    .and_then(Value::as_int)
            })
            .max()
            .unwrap_or(0)
            .max(0) as u32;
        if self.attempts.get(job).copied().unwrap_or(1) > retries {
            return Ok(None);
        }
        let jobs = members.iter().map(|(job, _)| *job).collect::<Vec<_>>();
        self.restore_updates(&jobs)?;
        let planner = self
            .planner
            .as_ref()
            .ok_or_else(|| DriverError::new("the Snakemake session has not planned its jobs"))?;
        let services = crate::services::service_files(&planner.jobs[*job]);
        if services.is_empty() || !crate::pipes::files(planner, &jobs).is_empty() {
            return self.rule_group(&jobs, Some(id));
        }
        self.service_group(*job, &services, Some(id))
    }

    fn rule_group(
        &mut self,
        jobs: &[usize],
        retry: Option<TaskId>,
    ) -> Result<Option<Task>, DriverError> {
        let planner = self
            .planner
            .as_ref()
            .ok_or_else(|| DriverError::new("No Snakemake plan"))?;
        if retry.is_none()
            && jobs.iter().any(|job| {
                planner.jobs[*job].incomplete
                    || self.tasks.contains_key(job)
                    || planner.jobs[*job].dependencies.iter().any(|(_, producer)| {
                        !jobs.contains(producer)
                            && self.needrun.get(producer).copied().unwrap_or(false)
                            && !self.done.contains(producer)
                    })
            })
        {
            return Ok(None);
        }
        let id = retry.unwrap_or_else(|| {
            let id = TaskId(self.next_task.get());
            self.next_task.set(id.0 + 1);
            id
        });
        let attempt = retry.map_or(1, |_| self.attempts.get(&jobs[0]).copied().unwrap_or(1) + 1);
        let mut members = Vec::new();
        let mut declared = Vec::new();
        let mut usages = Vec::new();
        for job in jobs {
            let entry = &planner.jobs[*job];
            let state = self.state.borrow();
            let setting = crate::task::TaskSetting {
                state: &state,
                files: &self.files,
                workdir: &self.workdir,
                embedded: &self.embedded,
                sources: &self.sources,
            };
            let crate::task::Built {
                task,
                signature,
                cwl,
                usage,
            } = crate::task::build(
                &mut self.interpreter,
                &setting,
                &planner.rules[entry.rule],
                entry,
                id,
                attempt,
            )
            .map_err(|error| driver_error(&error, &self.workdir))?;
            if cwl.is_some() {
                return Err(cwl_grouped());
            }
            self.signatures.insert(*job, signature);
            declared.push((*job, task.outputs.clone()));
            members.push(task);
            usages.push(usage);
        }
        let pipes = crate::pipes::files(planner, jobs);
        let services: Vec<_> = jobs
            .iter()
            .flat_map(|job| crate::services::service_files(&planner.jobs[*job]))
            .collect();
        if members
            .iter()
            .any(|member| member.image != members[0].image)
        {
            if !pipes.is_empty() {
                let mut required = HashMap::new();
                crate::pipes::components(planner, &self.order, &mut self.needrun, &mut required)
                    .map_err(DriverError::new)?;
                let conflict = jobs.iter().enumerate().any(|(index, job)| {
                    required.get(job).is_some_and(|component| {
                        component.iter().any(|peer| {
                            jobs.iter()
                                .position(|candidate| candidate == peer)
                                .is_some_and(|other| members[other].image != members[index].image)
                        })
                    })
                });
                if conflict {
                    return Err(DriverError::unsupported(
                        "Snakemake pipe jobs must run together and require the same container image",
                    ));
                }
                for job in jobs {
                    self.components.remove(job);
                    if let Some(component) = required.get(job) {
                        self.components.insert(*job, component.clone());
                    }
                }
                if let Some(component) = required.get(&jobs[0]) {
                    return self.rule_group(component, retry);
                }
                return Ok(None);
            }
            tracing::debug!(jobs = ?jobs, "Snakemake group uses different images; running separate tasks");
            for job in jobs {
                self.components.remove(job);
            }
            return Ok(None);
        }
        // Jobs joined by pipes or services run at once; the others in layers.
        let layers = match pipes.is_empty() && services.is_empty() {
            true => crate::groups::layers(planner, jobs),
            false => vec![0; jobs.len()],
        };
        let usage = crate::scheduling::combined(&usages, &layers);
        if !self.admit(jobs[0], retry.is_some(), &usage) {
            return Ok(None);
        }
        let combine = |members: &mut Vec<Task>| {
            crate::groups::combine(members, id, &pipes, &services).map_err(DriverError::new)
        };
        let Some(mut task) = self.share(jobs, members, combine)? else {
            return Ok(None);
        };
        let usage = self.tallied(usage);
        self.gate.take(id, usage);
        if retry.is_some() || jobs.iter().any(|job| self.forced.contains(job)) {
            task.reuse = workflow_graph::Reuse::Denied;
        }
        for job in jobs {
            self.planner.as_mut().unwrap().fixed.insert(*job);
            self.tasks.insert(*job, id);
            self.attempts.insert(*job, attempt);
        }
        self.jobs_of_tasks.insert(id, jobs[0]);
        self.groups.insert(id, declared);
        self.outstanding += 1;
        Ok(Some(task))
    }

    /// Whether the run ends after a failed job: the running jobs end, and no other job starts.
    pub fn draining(&self) -> bool {
        !self.failures.is_empty() && !self.launch.keep_going
    }

    /// The name of the rule of a job.
    fn rule_name(&self, job: usize) -> String {
        (self.planner.as_ref())
            .map(|planner| planner.rules[planner.jobs[job].rule].name.clone())
            .unwrap_or_default()
    }

    /// Notes a job that failed. Snakemake starts no job after it, and with `--keep-going` the
    /// jobs that do not depend on it; so the jobs after it never become ready either way. A
    /// run that keeps going plans again when a checkpoint waited for this job.
    fn failed(&mut self, message: String) -> Result<Vec<Task>, DriverError> {
        self.failures.push(message);
        if self.launch.keep_going && self.replan && self.outstanding == 0 {
            self.replan = false;
            self.plan()?;
            return self.ready_tasks(None);
        }
        // A run that keeps going starts the held jobs that the failed job made room for.
        if self.launch.keep_going && !self.held.is_empty() {
            return self.ready_tasks(None);
        }
        Ok(Vec::new())
    }

    /// Makes the `touch()` outputs that the job did not make, as Snakemake touches them after
    /// the job, and fails as Snakemake does when other outputs are missing.
    fn check_outputs(&mut self, job: usize, outcome: &mut Outcome) -> Result<(), DriverError> {
        let planner = self
            .planner
            .as_ref()
            .ok_or_else(|| DriverError::new("the Snakemake session has not planned its jobs"))?;
        let entry = &planner.jobs[job];
        let rule = planner.rules[entry.rule].name.clone();
        let touched = entry.touched_files();
        let directories: Vec<String> = (entry.output.values.iter())
            .filter(|value| value.tag("directory").is_some())
            .filter_map(|value| value.as_str().map(str::to_owned))
            .collect();
        let ensured: Vec<(String, Value)> = entry
            .output
            .values
            .iter()
            .filter_map(|value| Some((value.as_str()?.to_owned(), value.tag("ensure")?.clone())))
            .collect();
        let wildcards = crate::objects::NamedList::wildcards(&entry.wildcards);
        // Pipes and services do not become stored output files.
        let outputs: Vec<String> = (entry.output.values.iter())
            .filter(|value| value.tag("service").is_none() && value.tag("pipe").is_none())
            .filter_map(|value| value.as_str().map(str::to_owned))
            .collect();
        let made = |outcome: &Outcome, output: &str| {
            let output = output.trim_start_matches('/').trim_end_matches('/');
            (outcome.files.iter())
                .any(|file| file.path.trim_start_matches('/').trim_end_matches('/') == output)
        };
        let mut missing = Vec::new();
        for output in &outputs {
            if made(outcome, output) {
                continue;
            }
            if touched.iter().any(|file| file == output) {
                let directory = directories.contains(output);
                let location = self.touch(output, directory).map_err(|error| {
                    DriverError::new(format!("cannot touch {output} of rule {rule}: {error}"))
                })?;
                outcome.files.push(workflow_graph::OutputFile {
                    path: output.clone(),
                    location: location.to_string_lossy().into_owned(),
                    ..workflow_graph::OutputFile::default()
                });
                continue;
            }
            missing.push(output.clone());
        }
        if missing.is_empty()
            && self.shared
            && let Some(planner) = &self.planner
        {
            let improper = crate::shared::settle(&self.workdir, &planner.jobs[job])
                .map_err(|error| DriverError::new(format!("cannot touch outputs: {error}")))?;
            if !improper.is_empty() {
                return Err(DriverError::new(format!(
                    "ImproperOutputException in rule {rule}: Outputs of incorrect type \
                     (directories when expecting files or vice versa). Output directories must \
                     be flagged with directory(): {}",
                    improper.join(", ")
                )));
            }
        }
        if missing.is_empty() {
            for (output, options) in ensured {
                let Value::Dict(options) = options.plain() else {
                    continue;
                };
                let options = options.borrow().clone();
                let file = outcome
                    .files
                    .iter()
                    .find(|file| {
                        file.path.trim_start_matches('/').trim_end_matches('/')
                            == output.trim_start_matches('/').trim_end_matches('/')
                    })
                    .ok_or_else(|| {
                        DriverError::new(format!("ensure output {output} is missing"))
                    })?;
                let path = self
                    .files
                    .local_location(Path::new(&file.location))
                    .map_err(|error| DriverError::new(format!("cannot check {output}: {error}")))?;
                let non_empty = options.get("non_empty").cloned().unwrap_or(Value::None);
                if self
                    .interpreter
                    .truthy(&non_empty)
                    .map_err(|error| self.error(&error))?
                    && std::fs::metadata(&path)
                        .map_err(|error| DriverError::new(error.to_string()))?
                        .len()
                        == 0
                {
                    return Err(DriverError::new(format!(
                        "EnsureException in rule {rule}: {output} is empty"
                    )));
                }
                let mut expected = Vec::new();
                for algorithm in ["sha256", "md5", "sha1"] {
                    let value = options.get(algorithm).cloned().unwrap_or(Value::None);
                    if matches!(value.plain(), Value::None) {
                        continue;
                    }
                    let value =
                        crate::task::evaluate(&mut self.interpreter, &value, &wildcards, &[])
                            .map_err(|error| self.error(&error))?;
                    let checksum = value.as_str().ok_or_else(|| DriverError::new(format!(
                        "EnsureException in rule {rule}: {algorithm} for {output} must be a string"
                    )))?;
                    expected.push((algorithm, checksum.to_owned()));
                }
                if !expected.is_empty() {
                    let checksums = output_checksums(&path).map_err(|error| {
                        DriverError::new(format!("cannot checksum {output}: {error}"))
                    })?;
                    for (algorithm, expected) in expected {
                        if checksums
                            .iter()
                            .find(|(name, _)| *name == algorithm)
                            .is_none_or(|(_, checksum)| checksum != &expected)
                        {
                            return Err(DriverError::new(format!(
                                "EnsureException in rule {rule}: {algorithm} checksum mismatch for {output}"
                            )));
                        }
                    }
                }
            }
            return Ok(());
        }
        Err(DriverError::new(format!(
            "MissingOutputException in rule {rule}: Job {} completed successfully, but some \
             output files are missing: {}",
            outcome.id,
            missing.join(", ")
        )))
    }

    /// An empty file for a `touch()` output in the scratch folder of the run, or in place in
    /// the shared directory.
    fn touch(&mut self, output: &str, directory: bool) -> std::io::Result<PathBuf> {
        if self.shared {
            let path = self.workdir.join(output);
            crate::shared::touch(&path, directory)?;
            return Ok(path);
        }
        let folder = self.touched.get_or_insert_with(|| {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |time| time.as_nanos());
            self.launch
                .scratch
                .join("snakemake")
                .join("touched")
                .join(format!("{}-{nanos}", std::process::id()))
        });
        let path = folder.join(output.trim_start_matches('/'));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, b"")?;
        Ok(path)
    }

    /// The benchmark file of a finished job, which Snakemake writes with the time the job took
    /// and what it used, unless the job wrote it itself.
    fn benchmark(&mut self, job: usize, outcome: &Outcome) -> Result<(), DriverError> {
        let name = (self.planner.as_ref())
            .and_then(|planner| planner.jobs[job].benchmark.as_ref())
            .and_then(|value| value.as_str().map(str::to_owned));
        let Some(name) = name else {
            return Ok(());
        };
        let path = self.interpreter.host.resolve(&name);
        if (outcome.files.iter()).any(|file| self.interpreter.host.resolve(&file.path) == path) {
            return Ok(());
        }
        let file = self
            .launch
            .scratch
            .join(format!("benchmark-{}", outcome.id));
        std::fs::create_dir_all(&self.launch.scratch)
            .and_then(|()| std::fs::write(&file, crate::benchmark::text(outcome, &name)))
            .map_err(|error| DriverError::new(error.to_string()))?;
        let mut deliveries = self
            .deliveries
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        deliveries.push(Delivery {
            location: file.to_string_lossy().into_owned(),
            target: path.to_string_lossy().into_owned(),
            mode: DeliveryMode::Copy,
        });
        Ok(())
    }

    /// Snakemake's `handle_log`: a log file that a finished job did not write is delivered
    /// empty.
    fn empty_logs(&mut self, job: usize, outcome: &Outcome) -> Result<(), DriverError> {
        let logs: Vec<String> = (self.planner.as_ref())
            .map(|planner| {
                (planner.jobs[job].log.values.iter())
                    .filter_map(|value| value.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let written: Vec<PathBuf> = (outcome.files.iter())
            .map(|file| self.interpreter.host.resolve(&file.path))
            .collect();
        for log in logs {
            let path = self.interpreter.host.resolve(&log);
            if written.contains(&path) {
                continue;
            }
            let empty = self.launch.scratch.join("empty-log");
            std::fs::create_dir_all(&self.launch.scratch)
                .and_then(|()| std::fs::write(&empty, ""))
                .map_err(|error| DriverError::new(error.to_string()))?;
            let mut deliveries = self
                .deliveries
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            deliveries.push(Delivery {
                location: empty.to_string_lossy().into_owned(),
                target: path.to_string_lossy().into_owned(),
                mode: DeliveryMode::Copy,
            });
        }
        Ok(())
    }

    /// The next attempt of a failed job, while its rule's `retries` allow one, with its
    /// resources evaluated for that attempt, as Snakemake runs it.
    fn retry(&mut self, job: usize, id: TaskId) -> Result<Option<Task>, DriverError> {
        let planner = self
            .planner
            .as_ref()
            .ok_or_else(|| DriverError::new("the Snakemake session has not planned its jobs"))?;
        let rule = planner.rules[planner.jobs[job].rule].clone();
        let entry = planner.jobs[job].clone();
        let attempt = self.attempts.get(&job).copied().unwrap_or(1);
        let retries = (rule.retries.as_ref())
            .and_then(Value::as_int)
            .map_or(0, |retries| retries.max(0) as u32);
        // The CWL run of a `cwl:` job does not start again.
        if attempt > retries || matches!(rule.action, crate::rule::Action::Cwl(_)) {
            return Ok(None);
        }
        self.restore_updates(&[job])?;
        eprintln!(
            "Trying to restart job of rule {} (attempt {}).",
            rule.name,
            attempt + 1
        );
        let crate::task::Built {
            task, signature, ..
        } = {
            let state = self.state.borrow();
            let setting = crate::task::TaskSetting {
                state: &state,
                files: &self.files,
                workdir: &self.workdir,
                embedded: &self.embedded,
                sources: &self.sources,
            };
            crate::task::build(
                &mut self.interpreter,
                &setting,
                &rule,
                &entry,
                id,
                attempt + 1,
            )
        }
        .map_err(|error| self.error(&error))?;
        self.signatures.insert(job, signature);
        let Some(mut task) = self.share(&[job], vec![task], |members| Ok(members.remove(0)))?
        else {
            return Ok(None);
        };
        task.reuse = workflow_graph::Reuse::Denied;
        self.attempts.insert(job, attempt + 1);
        self.outstanding += 1;
        Ok(Some(task))
    }

    pub fn finish(&mut self) -> Result<(), DriverError> {
        if let Some(first) = self.failures.first() {
            let message = match self.failures.len() {
                1 => first.clone(),
                count => format!("{first}; and {} more jobs failed", count - 1),
            };
            return Err(DriverError::new(message));
        }
        let Some(planner) = &self.planner else {
            return Ok(());
        };
        let waiting: Vec<String> = self
            .needrun
            .iter()
            .filter(|(job, needed)| **needed && !self.done.contains(job))
            .map(|(job, _)| planner.rules[planner.jobs[*job].rule].name.clone())
            .collect();
        if !waiting.is_empty() {
            return Err(DriverError::new(format!(
                "{} jobs did not run, like a job of rule {}",
                waiting.len(),
                waiting[0]
            )));
        }
        self.release_temporary(&[])?;
        if self.shared {
            self.clean_folders();
        }
        if self.hooks_started && !self.hooks_finished {
            self.hooks_finished = true;
            self.run_hook(GlobalKeyword::OnSuccess)?;
        }
        self.interpreter.packages.stop();
        Ok(())
    }
}

/// Why a `cwl:` job cannot run in a pipe or service group: its CWL run has tasks of its own.
fn cwl_grouped() -> DriverError {
    DriverError::unsupported(
        "a cwl rule in a pipe or service: iunctus runs the CWL document of a cwl rule as tasks of their own",
    )
}

fn output_checksums(path: &Path) -> std::io::Result<[(&'static str, String); 3]> {
    let mut input = BufReader::new(std::fs::File::open(path)?);
    let mut sha256 = sha2::Sha256::new();
    let mut md5 = md5::Md5::new();
    let mut sha1 = sha1::Sha1::new();
    loop {
        let bytes = input.fill_buf()?;
        if bytes.is_empty() {
            break;
        }
        sha256.update(bytes);
        md5.update(bytes);
        sha1.update(bytes);
        let length = bytes.len();
        input.consume(length);
    }
    let hex = |bytes: &[u8]| bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok([
        ("sha256", hex(&sha256.finalize())),
        ("md5", hex(&md5.finalize())),
        ("sha1", hex(&sha1.finalize())),
    ])
}

/// The driver the engine talks to. The session lives on a thread of its own with a large
/// stack, since workflow code may recurse deeply.
struct SnakemakeDriver {
    requests: Sender<Request>,
    replies: Receiver<Reply>,
    /// The answers to requests for the inputs.
    inputs: Receiver<InputsReply>,
    /// The answers to requests for the outputs.
    outputs: Receiver<OutputsReply>,
    worker: Option<JoinHandle<()>>,
    /// The notes of the session, which it adds before each reply.
    notes: Arc<Mutex<Vec<Note>>>,
    /// The outputs that the session asked to deliver.
    deliveries: Arc<Mutex<Vec<Delivery>>>,
    /// The files of the tasks, which the session reads outputs through.
    files: Arc<OnceLock<Arc<dyn Files>>>,
    /// Whether the session drains the run after a failed job, as of its last reply.
    draining: Arc<AtomicBool>,
    acceptance: Arc<Mutex<HashSet<(TaskId, u32)>>>,
}

impl SnakemakeDriver {
    fn ask(&mut self, request: Request) -> Reply {
        if self.requests.send(request).is_err() {
            return Err(DriverError::new("the Snakemake session ended unexpectedly"));
        }
        self.replies
            .recv()
            .unwrap_or_else(|_| Err(DriverError::new("the Snakemake session ended unexpectedly")))
    }
}

impl Driver for SnakemakeDriver {
    fn language(&self) -> Language {
        Language::Snakemake
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        self.ask(Request::Start)
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        self.ask(Request::Complete(Box::new(outcome)))
    }

    fn take_acceptance(&mut self, task: TaskId, attempt: u32) -> bool {
        let mut acceptance = self
            .acceptance
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        acceptance.remove(&(task, attempt))
    }

    fn notes(&mut self) -> Vec<Note> {
        self.notes
            .lock()
            .map(|mut notes| std::mem::take(&mut *notes))
            .unwrap_or_default()
    }

    fn deliveries(&mut self) -> Vec<Delivery> {
        let mut deliveries = self
            .deliveries
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        std::mem::take(&mut deliveries)
    }

    fn delivered(&mut self, delivery: &Delivery) -> Result<(), DriverError> {
        self.ask(Request::Delivered(delivery.clone())).map(|_| ())
    }

    fn use_files(&mut self, files: Arc<dyn Files>) {
        let _ = self.files.set(files);
    }

    fn draining(&self) -> bool {
        self.draining.load(Ordering::Relaxed)
    }

    fn inputs(&mut self) -> Result<Option<Inputs>, DriverError> {
        let ended = || DriverError::new("the Snakemake session ended unexpectedly");
        self.requests.send(Request::Inputs).map_err(|_| ended())?;
        self.inputs.recv().unwrap_or_else(|_| Err(ended()))
    }

    fn outputs(&mut self) -> Result<Option<serde_json::Value>, DriverError> {
        let ended = || DriverError::new("the Snakemake session ended unexpectedly");
        self.requests.send(Request::Outputs).map_err(|_| ended())?;
        self.outputs.recv().unwrap_or_else(|_| Err(ended()))
    }

    fn finish(&mut self) -> Result<(), DriverError> {
        let result = self.ask(Request::Finish).map(|_| ());
        if result.is_ok()
            && let Some(worker) = self.worker.take()
        {
            let _ = worker.join();
        }
        result
    }

    fn failed(&mut self, error: &workflow_graph::ExecuteError) -> Result<(), DriverError> {
        if self.worker.is_none() {
            return Ok(());
        }
        let result = self.ask(Request::Failed(error.to_string())).map(|_| ());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        result
    }
}

impl Drop for SnakemakeDriver {
    fn drop(&mut self) {
        let (closed, _) = channel();
        self.requests = closed;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// What a Snakemake run may use besides its launch.
#[derive(Clone, Debug, Default)]
pub struct Settings {
    /// The Python container of decision 19; without it, an import of a package without a
    /// stand-in stops the run.
    pub python: Option<PythonContainers>,
    /// The storage providers that files of the workflow on the web come from, like modules
    /// from a private server; the built-in ones without it.
    pub storage: Storage,
    pub cancel: executor::CancellationToken,
    /// The Git client for Snakefiles of `gitfile(...)` in repositories on the web.
    pub git: Option<crate::gitfile::GitClient>,
}

/// Runtime options supplied by the CLI in addition to workflow profile defaults.
#[derive(Clone, Debug, Default)]
pub struct RunSettings {
    pub image: Option<String>,
    pub offline: bool,
    pub rerun_triggers: Option<Vec<String>>,
    pub conda: bool,
    /// Whether the executor sees the working directory at its path, so that jobs run in it
    /// as in Snakemake; a simulated run never does.
    pub shared_workdir: bool,
    /// Whether jobs run on this machine, as Snakemake's local runs, which form no group jobs.
    pub local: bool,
}

/// The driver of a Snakemake workflow: `launch.target` is a Snakefile, or a folder with a
/// `Snakefile` or `workflow/Snakefile`, which is then the working directory.
pub fn driver(launch: &Launch) -> Result<Box<dyn Driver>, DriverError> {
    driver_with(launch, Settings::default())
}

/// The driver of a Snakemake workflow with settings, like the Python container.
pub fn driver_with(launch: &Launch, settings: Settings) -> Result<Box<dyn Driver>, DriverError> {
    driver_with_options(launch, settings, RunSettings::default())
}

pub fn driver_with_options(
    launch: &Launch,
    settings: Settings,
    options: RunSettings,
) -> Result<Box<dyn Driver>, DriverError> {
    let (requests, incoming) = channel::<Request>();
    let (outgoing, replies) = channel::<Reply>();
    let (inputs_outgoing, inputs) = channel::<InputsReply>();
    let (outputs_outgoing, outputs) = channel::<OutputsReply>();
    let launch = launch.clone();
    let notes: Arc<Mutex<Vec<Note>>> = Arc::default();
    let session_notes = notes.clone();
    let deliveries: Arc<Mutex<Vec<Delivery>>> = Arc::default();
    let shared = deliveries.clone();
    let files: Arc<OnceLock<Arc<dyn Files>>> = Arc::default();
    let task_files = files.clone();
    let draining: Arc<AtomicBool> = Arc::default();
    let session_draining = draining.clone();
    let acceptance: Arc<Mutex<HashSet<(TaskId, u32)>>> = Arc::default();
    let session_acceptance = acceptance.clone();
    let worker = std::thread::Builder::new()
        .name("snakemake".into())
        .stack_size(512 << 20)
        .spawn(move || {
            let channel = Rc::new(Channel::new(incoming, outgoing));
            let open = || -> Result<Session, DriverError> {
                let mut session = Session::new(&launch, &settings.storage)?;
                *session.state.borrow().mirrors.git.borrow_mut() = settings.git.clone();
                session.rerun_triggers = options.rerun_triggers.clone();
                session.shared = options.shared_workdir && !launch.simulated;
                session.local = options.local;
                session.embedded.local = options.local;
                session.embedded.shared = session.shared;
                session.state.borrow_mut().conda_enabled = options.conda;
                session.state.borrow_mut().default_image = options.image.clone();
                session.acceptance = session_acceptance.clone();
                session.deliveries = shared.clone();
                session.files.read_outputs_through(task_files.clone());
                if let Some(python) = settings.python.clone() {
                    session.use_cancellable_python(python, settings.cancel.clone());
                }
                session.files.set_commands(Rc::new(CommandRunner {
                    channel: channel.clone(),
                    next_task: session.next_task.clone(),
                    state: session.state.clone(),
                    settings: options.clone(),
                    engine: settings
                        .python
                        .as_ref()
                        .map(|python| python.engine.clone())
                        .unwrap_or_default(),
                    scratch: launch.scratch.clone(),
                    simulated: launch.simulated,
                    cancel: settings.cancel.clone(),
                }));
                Ok(session)
            };
            let mut session = match open() {
                Ok(session) => session,
                Err(error) => {
                    // Every request gets the error of the setup.
                    while let Ok(request) = channel.requests.recv() {
                        let _ = match request {
                            Request::Inputs => inputs_outgoing.send(Err(error.clone())).is_ok(),
                            Request::Outputs => outputs_outgoing.send(Err(error.clone())).is_ok(),
                            Request::Finish | Request::Failed(_) => {
                                let _ = channel.replies.send(Err(error.clone()));
                                break;
                            }
                            _ => channel.replies.send(Err(error.clone())).is_ok(),
                        };
                    }
                    return;
                }
            };
            let session = &mut session;
            while let Ok(request) = channel.requests.recv() {
                let mut reply = match request {
                    Request::Inputs => {
                        let described = describe(session, &channel, &open, Session::inputs);
                        if let Ok(mut notes) = session_notes.lock() {
                            notes.extend(session.take_notes());
                        }
                        if inputs_outgoing.send(described).is_err() {
                            break;
                        }
                        continue;
                    }
                    Request::Outputs => {
                        let described = describe(session, &channel, &open, Session::outputs);
                        if let Ok(mut notes) = session_notes.lock() {
                            notes.extend(session.take_notes());
                        }
                        if outputs_outgoing.send(described).is_err() {
                            break;
                        }
                        continue;
                    }
                    Request::Start => session.start(),
                    Request::Complete(outcome) => session.complete(*outcome),
                    Request::Delivered(delivery) => {
                        session.delivered(&delivery).map(|()| Vec::new())
                    }
                    Request::Finish => {
                        let result = session.finish().map(|()| Vec::new());
                        let finished = result.is_ok();
                        let _ = channel.replies.send(result);
                        if finished {
                            break;
                        }
                        continue;
                    }
                    Request::Failed(error) => {
                        let result = session.failure_hook(&error).map(|()| Vec::new());
                        let _ = channel.replies.send(result);
                        break;
                    }
                };
                if let Some(error) = channel.failed.borrow_mut().take() {
                    let result = session.failure_hook(&error).map(|()| Vec::new());
                    let _ = channel.replies.send(result);
                    break;
                }
                // The outcomes that came while commands of workflow code ran.
                reply = reply.and_then(|mut tasks| {
                    loop {
                        let parked = channel.parked.borrow_mut().pop_front();
                        let Some(outcome) = parked else {
                            break;
                        };
                        tasks.extend(session.complete(outcome)?);
                    }
                    Ok(tasks)
                });
                while let Some(delivery) = { channel.parked_deliveries.borrow_mut().pop_front() } {
                    let acknowledged = session.delivered(&delivery);
                    if reply.is_ok()
                        && let Err(error) = acknowledged
                    {
                        reply = Err(error);
                    }
                }
                session.stop_unneeded_python();
                if let Ok(mut notes) = session_notes.lock() {
                    notes.extend(session.take_notes());
                }
                session_draining.store(session.draining(), Ordering::Relaxed);
                if channel.replies.send(reply).is_err() {
                    break;
                }
            }
        })
        .map_err(|error| {
            DriverError::new(format!("cannot start the Snakemake session: {error}"))
        })?;
    Ok(Box::new(SnakemakeDriver {
        requests,
        replies,
        inputs,
        outputs,
        worker: Some(worker),
        notes,
        deliveries,
        files,
        draining,
        acceptance,
    }))
}

/// The inputs or outputs of the workflow of a session. Workflow code that runs a command while
/// the workflow is loaded needs a run, which runs the command as a task: such a workflow has no
/// inputs or outputs to describe before the run, and the session starts again for the run.
fn describe<T>(
    session: &mut Session,
    channel: &Channel,
    open: &dyn Fn() -> Result<Session, DriverError>,
    what: impl FnOnce(&mut Session) -> Result<T, DriverError>,
) -> Result<Option<T>, DriverError> {
    channel.describing.set(true);
    let described = what(session);
    channel.describing.set(false);
    if !channel.refused.replace(false) {
        return described.map(Some);
    }
    let _ = session.finish();
    *session = open()?;
    Ok(None)
}

/// Snakemake's `handle_pipes_and_services`: a pipe or service output of a job that runs has
/// one consumer (a pipe exactly one), which runs something, and neither side is a `run:`
/// block; the first problem, if any, in Snakemake's words.
fn pipe_problem(
    planner: &crate::plan::Planner,
    order: &[usize],
    needrun: &HashMap<usize, bool>,
) -> Option<String> {
    use crate::rule::Action;
    let action = |job: usize| &planner.rules[planner.jobs[job].rule].action;
    for job in order.iter().copied() {
        if !needrun.get(&job).copied().unwrap_or(false) {
            continue;
        }
        for value in &planner.jobs[job].output.values {
            let pipe = value.tag("pipe").is_some();
            if !pipe && value.tag("service").is_none() {
                continue;
            }
            let Some(file) = value.as_str() else {
                continue;
            };
            if matches!(action(job), Action::Run(_)) {
                return Some(
                    "Rule defines pipe output but uses a 'run' directive. This is not possible \
                     for technical reasons. Consider using 'shell' or 'script'."
                        .into(),
                );
            }
            let consumers: Vec<usize> = (order.iter().copied())
                .filter(|consumer| {
                    (planner.jobs[*consumer].dependencies.iter())
                        .any(|(input, producer)| *producer == job && input == file)
                })
                .collect();
            match consumers.as_slice() {
                [_, _, ..] if pipe => {
                    return Some(format!(
                        "Output file {file} is marked as pipe but more than one job depends on \
                         it. Make sure that any pipe output is only consumed by one job"
                    ));
                }
                [] => {
                    return Some(format!(
                        "Output file {file} is marked as pipe or service but it has no \
                         consumer. This is invalid because it can lead to a dead lock."
                    ));
                }
                [first, ..] if pipe && matches!(action(*first), Action::None) => {
                    return Some(format!(
                        "Output file {file} is marked as pipe but is requested by a rule that \
                         does not execute anything. This is not allowed because it would lead \
                         to a dead lock."
                    ));
                }
                _ => {}
            }
            if consumers
                .iter()
                .any(|consumer| matches!(action(*consumer), Action::Run(_)))
            {
                return Some(
                    "Rule consumes pipe or service input but uses a 'run' directive. This is \
                     not possible for technical reasons. Consider using 'shell' or 'script'."
                        .into(),
                );
            }
        }
    }
    None
}
