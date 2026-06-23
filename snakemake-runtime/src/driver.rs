//! The Snakemake driver: evaluates the Snakefile, plans the jobs of the targets, and asks the
//! engine for the task of each job once the jobs it depends on are done. After a checkpoint's
//! task, the workflow is planned again with the checkpoint's outputs.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use python_evaluator::{Error, Interpreter, Value};
use workflow_graph::{Driver, DriverError, Language, Launch, Outcome, Task, TaskId};

use crate::commands::{Channel, CommandRunner, Reply, Request};
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
}

impl Session {
    pub fn new(launch: &Launch) -> Result<Session, DriverError> {
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
        })
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
        if self.launch.assume_inputs
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
        for (name, value) in &params {
            let name = name.trim_start_matches('-').to_owned();
            if let Value::Dict(dict) = overwrite.plain() {
                dict.borrow_mut()
                    .insert(&name, crate::config::parse_value(value));
            }
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
                self.launch.assume_inputs,
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
        self.needrun = planner.needrun(&self.interpreter, &roots);
        self.order = planner.ordered_jobs(&roots);
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
            let task = {
                let state = self.state.borrow();
                let setting = crate::task::TaskSetting {
                    state: &state,
                    files: &self.files,
                    workdir: &self.workdir,
                };
                crate::task::build(&mut self.interpreter, &setting, &rule, &entry, id)
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
            self.jobs_of_tasks.insert(id, job);
            self.outstanding += 1;
            tasks.push(task);
        }
        Ok(tasks)
    }

    /// Runs the Python packages without a stand-in in a container of the local engine
    /// (decision 19), which sees the folders of the workflow.
    pub fn use_python_containers(&mut self, settings: PythonContainers) {
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

    pub fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        self.evaluate()?;
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
        if outcome.exit_code != 0 {
            let name = self
                .planner
                .as_ref()
                .map(|planner| planner.rules[planner.jobs[job].rule].name.clone())
                .unwrap_or_default();
            return Err(DriverError::new(format!(
                "Error in rule {name}: the task ended with exit code {}",
                outcome.exit_code
            )));
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
            self.files.add_output(
                &path,
                Produced {
                    location: file.location.clone(),
                    directory: file.directory,
                },
            );
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
        if self.replan && self.outstanding == 0 {
            self.replan = false;
            self.plan()?;
            return self.ready_tasks(None);
        }
        self.ready_tasks(Some(job))
    }

    pub fn finish(&mut self) -> Result<(), DriverError> {
        self.interpreter.packages.stop();
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
    worker: Option<JoinHandle<()>>,
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
    let launch = launch.clone();
    let worker = std::thread::Builder::new()
        .name("snakemake".into())
        .stack_size(512 << 20)
        .spawn(move || {
            let channel = Rc::new(Channel::new(incoming, outgoing));
            let mut session = match Session::new(&launch) {
                Ok(session) => Some(session),
                Err(error) => {
                    // Every request gets the error of the setup.
                    while let Ok(request) = channel.requests.recv() {
                        let finish = matches!(request, Request::Finish);
                        let _ = channel.replies.send(Err(error.clone()));
                        if finish {
                            break;
                        }
                    }
                    None
                }
            };
            let Some(session) = session.as_mut() else {
                return;
            };
            if let Some(python) = settings.python {
                session.use_python_containers(python);
            }
            session.files.set_commands(Rc::new(CommandRunner {
                channel: channel.clone(),
                next_task: session.next_task.clone(),
                state: session.state.clone(),
            }));
            while let Ok(request) = channel.requests.recv() {
                let reply = match request {
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
        worker: Some(worker),
    }))
}
