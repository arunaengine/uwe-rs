//! The Snakemake driver: evaluates the Snakefile, plans the jobs of the targets, and asks the
//! engine for the task of each job once the jobs it depends on are done. After a checkpoint's
//! task, the workflow is planned again with the checkpoint's outputs.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread::JoinHandle;

use python_evaluator::{Error, Interpreter, Value};
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
    tasks: HashMap<usize, TaskId>,
    jobs_of_tasks: HashMap<TaskId, usize>,
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
        let mut interpreter = Interpreter::new(files.clone());
        interpreter.stack_limit = 480 << 20;
        let globals = interpreter.new_globals("snakemake.workflow", None);
        let state = Rc::new(RefCell::new(State::new(
            globals.clone(),
            snakefile.clone(),
            workdir.clone(),
            mirrors,
        )));
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
            tasks: HashMap::new(),
            jobs_of_tasks: HashMap::new(),
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
            kept: Vec::new(),
            deliveries: Arc::default(),
            evaluation: None,
            embedded: crate::embedded::Setting::new(&launch.scratch, launch.simulated),
            touched: None,
            sources: Rc::default(),
            failures: Vec::new(),
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
        let profile = if self.launch.profiles.is_empty() {
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
        self.state.borrow_mut().profile = profile;
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
        }
        let targets: Vec<String> = if self.launch.targets.is_empty() {
            self.state.borrow().default_target.iter().cloned().collect()
        } else {
            self.launch.targets.clone()
        };
        let planner = self.planner.as_mut().expect("a planner");
        planner.reset();
        let mut roots = Vec::new();
        for target in &targets {
            let is_rule = planner.rules.iter().any(|rule| rule.name == *target);
            let root = if is_rule {
                Some(planner.plan_rule(&mut self.interpreter, target))
            } else {
                planner
                    .plan_target_file(&mut self.interpreter, target)
                    .transpose()
            };
            match root {
                Some(Ok(job)) => roots.push(job),
                Some(Err(error)) => return Err(self.error(&error)),
                None => {}
            }
        }
        self.kept = (targets.iter())
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
        self.order = planner.ordered_jobs(&roots);
        // Snakemake's `check_directory_outputs`: no output of a job lies in a directory
        // output of another job.
        let mut outputs: Vec<(PathBuf, usize)> = (self.order.iter())
            .flat_map(|job| {
                (planner.jobs[*job].output_files().into_iter()).map(|file| {
                    (
                        self.interpreter.host.resolve(file.trim_end_matches('/')),
                        *job,
                    )
                })
            })
            .collect();
        outputs.sort();
        outputs.dedup();
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
                waiting.sort_by_key(|job| self.position.get(job).copied().unwrap_or(usize::MAX));
                waiting.dedup();
                waiting
            }
        };
        let mut tasks = Vec::new();
        for job in candidates {
            if !self.needrun.get(&job).copied().unwrap_or(false) || self.tasks.contains_key(&job) {
                continue;
            }
            let planner = self.planner.as_ref().expect("a planner");
            let entry = &planner.jobs[job];
            if entry.incomplete {
                continue;
            }
            if self.remaining.get(&job).copied().unwrap_or(0) > 0 {
                continue;
            }
            let id = TaskId(self.next_task.get());
            self.next_task.set(id.0 + 1);
            let rule = planner.rules[entry.rule].clone();
            let entry = entry.clone();
            if rule.checkpoint {
                let reason = format!(
                    "rule `{}` is a checkpoint: Snakemake plans the workflow again after it",
                    rule.name
                );
                let marker = Marker::new(MarkerKind::Checkpoint, reason);
                self.notes.push(Note::Mark(id, marker));
            }
            let task = {
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
            let mut task = task;
            // A job whose input function looked into a checkpoint's outputs runs after it,
            // even when it reads none of its files.
            let planner = self.planner.as_ref().expect("a planner");
            for file in &entry.checkpoints {
                for (other, other_task) in &self.tasks {
                    if planner.jobs[*other].output_files().contains(file)
                        && !task.after.contains(other_task)
                    {
                        task.after.push(*other_task);
                    }
                }
            }
            self.planner.as_mut().expect("a planner").fixed.insert(job);
            self.tasks.insert(job, id);
            self.attempts.insert(job, 1);
            self.jobs_of_tasks.insert(id, job);
            self.outstanding += 1;
            tasks.push(task);
        }
        Ok(tasks)
    }

    /// Runs the Python packages without a stand-in in a container of the local engine
    /// (decision 19), which sees the folders of the workflow.
    pub fn use_python_containers(&mut self, settings: PythonContainers) {
        self.embedded.python_image = settings.image.clone();
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
            .set_starter(Rc::new(ContainerStarter::new(settings, folders)));
    }

    /// Stops the Python container once no more Python code runs: every job that must run has
    /// its task, and no checkpoint job is left whose outputs make the workflow planned again.
    fn stop_python_when_done(&self) {
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
        self.ready_tasks(None)
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
        let Some(job) = self.jobs_of_tasks.get(&outcome.id).copied() else {
            return Err(DriverError::new(format!("unknown task {}", outcome.id)));
        };
        self.outstanding = self.outstanding.saturating_sub(1);
        // Only reads while this outcome is handled explain an error of it.
        *self.files.last_output_read.borrow_mut() = None;
        if outcome.exit_code != 0
            && let Some(task) = self.retry(job, outcome.id)?
        {
            return Ok(vec![task]);
        }
        if outcome.exit_code != 0 {
            let message = format!(
                "Error in rule {}: the task ended with exit code {}",
                self.rule_name(job),
                outcome.exit_code
            );
            return self.failed(message);
        }
        let mut outcome = outcome;
        if !self.launch.simulated
            && let Err(error) = self.check_outputs(job, &mut outcome)
        {
            return self.failed(error.message);
        }
        // The outputs go to the working directory, as Snakemake leaves them there, except the
        // temporary ones.
        let mut temporary = (self.planner.as_ref())
            .map(|planner| planner.jobs[job].temporary_files())
            .unwrap_or_default();
        temporary.retain(|file| !self.kept.contains(file));
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
            if !temporary
                .iter()
                .any(|output| self.interpreter.host.resolve(output) == path)
            {
                let mut deliveries = self
                    .deliveries
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                deliveries.push(Delivery {
                    location: file.location.clone(),
                    target: path.to_string_lossy().into_owned(),
                    mode: DeliveryMode::Copy,
                });
            }
            self.files.add_output(
                &path,
                Produced {
                    location: PathBuf::from(&file.location),
                    directory: file.directory,
                },
            );
        }
        if !self.launch.simulated {
            self.empty_logs(job, &outcome)?;
        }
        self.done.insert(job);
        if self.needrun.get(&job).copied().unwrap_or(false) {
            for dependent in self.dependents.get(&job).cloned().unwrap_or_default() {
                if let Some(count) = self.remaining.get_mut(&dependent) {
                    *count = count.saturating_sub(1);
                }
            }
        }
        let planner = self.planner.as_ref().expect("a planner");
        if planner.rules[planner.jobs[job].rule].checkpoint {
            self.replan = true;
        }
        // After a failure, running jobs end, but no job starts unless the run keeps going.
        if !self.failures.is_empty() && !self.launch.keep_going {
            return Ok(Vec::new());
        }
        if self.replan && self.outstanding == 0 {
            self.replan = false;
            self.plan()?;
            return self.ready_tasks(None);
        }
        self.ready_tasks(Some(job))
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
        Ok(Vec::new())
    }

    /// Makes the `touch()` outputs that the job did not make, as Snakemake touches them after
    /// the job, and fails as Snakemake does when other outputs are missing.
    fn check_outputs(&mut self, job: usize, outcome: &mut Outcome) -> Result<(), DriverError> {
        let planner = self.planner.as_ref().expect("a planner");
        let entry = &planner.jobs[job];
        let rule = planner.rules[entry.rule].name.clone();
        let touched = entry.touched_files();
        let outputs: Vec<String> = (entry.output.values.iter())
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
                let location = self.touch(output).map_err(|error| {
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
        if missing.is_empty() {
            return Ok(());
        }
        Err(DriverError::new(format!(
            "MissingOutputException in rule {rule}: Job {} completed successfully, but some \
             output files are missing: {}",
            outcome.id,
            missing.join(", ")
        )))
    }

    /// An empty file for a `touch()` output in the scratch folder of the run.
    fn touch(&mut self, output: &str) -> std::io::Result<PathBuf> {
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
        let planner = self.planner.as_ref().expect("a planner");
        let rule = planner.rules[planner.jobs[job].rule].clone();
        let entry = planner.jobs[job].clone();
        let attempt = self.attempts.get(&job).copied().unwrap_or(1);
        let retries = (rule.retries.as_ref())
            .and_then(Value::as_int)
            .map_or(0, |retries| retries.max(0) as u32);
        if attempt > retries {
            return Ok(None);
        }
        eprintln!(
            "Trying to restart job of rule {} (attempt {}).",
            rule.name,
            attempt + 1
        );
        let task = {
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
        self.attempts.insert(job, attempt + 1);
        self.outstanding += 1;
        Ok(Some(task))
    }

    pub fn finish(&mut self) -> Result<(), DriverError> {
        self.interpreter.packages.stop();
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
        Ok(())
    }
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
}

/// The driver of a Snakemake workflow: `launch.target` is a Snakefile, or a folder with a
/// `Snakefile` or `workflow/Snakefile`, which is then the working directory.
pub fn driver(launch: &Launch) -> Result<Box<dyn Driver>, DriverError> {
    driver_with(launch, Settings::default())
}

/// The driver of a Snakemake workflow with settings, like the Python container.
pub fn driver_with(launch: &Launch, settings: Settings) -> Result<Box<dyn Driver>, DriverError> {
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
    let worker = std::thread::Builder::new()
        .name("snakemake".into())
        .stack_size(512 << 20)
        .spawn(move || {
            let channel = Rc::new(Channel::new(incoming, outgoing));
            let open = || -> Result<Session, DriverError> {
                let mut session = Session::new(&launch, &settings.storage)?;
                session.deliveries = shared.clone();
                session.files.read_outputs_through(task_files.clone());
                if let Some(python) = settings.python.clone() {
                    session.use_python_containers(python);
                }
                session.files.set_commands(Rc::new(CommandRunner {
                    channel: channel.clone(),
                    next_task: session.next_task.clone(),
                    state: session.state.clone(),
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
                            Request::Finish => {
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
                let reply = match request {
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
                    Request::Finish => {
                        let result = session.finish().map(|()| Vec::new());
                        let _ = channel.replies.send(result);
                        break;
                    }
                };
                // The outcomes that came while commands of workflow code ran.
                let reply = reply.and_then(|mut tasks| {
                    loop {
                        let parked = channel.parked.borrow_mut().pop_front();
                        let Some(outcome) = parked else {
                            break;
                        };
                        tasks.extend(session.complete(outcome)?);
                    }
                    Ok(tasks)
                });
                session.stop_python_when_done();
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
