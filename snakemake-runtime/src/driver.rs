//! The Snakemake driver: evaluates the Snakefile, plans the jobs of the targets, and asks the
//! engine for the task of each job once the jobs it depends on are done. After a checkpoint's
//! task, the workflow is planned again with the checkpoint's outputs.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use python_evaluator::{Error, Interpreter, Value};
use workflow_graph::{Driver, DriverError, Language, Launch, Outcome, Task, TaskId};

use crate::files::{Produced, WorkflowFiles};
use crate::plan::Planner;
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
    let missing_module = error.is("ModuleNotFoundError");
    if error.is_unsupported() || missing_module {
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
    next_task: u64,
    replan: bool,
}

impl Session {
    pub fn new(launch: &Launch) -> Result<Session, DriverError> {
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
        let files = Rc::new(WorkflowFiles::new(&workdir));
        let mut interpreter = Interpreter::new(files.clone());
        interpreter.stack_limit = 480 << 20;
        let globals = interpreter.new_globals("snakemake.workflow", None);
        let state = Rc::new(RefCell::new(State::new(
            globals.clone(),
            snakefile.clone(),
            workdir.clone(),
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
            next_task: 1,
            replan: false,
        })
    }

    fn error(&self, error: &Error) -> DriverError {
        driver_error(error, &self.workdir)
    }

    /// Sets the config from the command line and runs the Snakefile.
    pub fn evaluate(&mut self) -> Result<(), DriverError> {
        let overwrite = Value::dict(python_evaluator::Dict::default());
        let mut configfiles = Vec::new();
        for path in self.launch.configs.iter().chain(self.launch.inputs.iter()) {
            let text = path.to_string_lossy().into_owned();
            let loaded = crate::config::load_config(&mut self.interpreter, &text)
                .map_err(|error| self.error(&error))?;
            crate::config::update(&mut self.interpreter, &overwrite, &loaded)
                .map_err(|error| self.error(&error))?;
            configfiles.push(text);
        }
        for (name, value) in &self.launch.params {
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
                Some(Err(error)) => return Err(driver_error(&error, &self.workdir)),
                None => {}
            }
        }
        self.needrun = planner.needrun(&self.interpreter, &roots);
        self.roots = roots;
        Ok(())
    }

    /// The tasks of the jobs that can run now.
    fn ready_tasks(&mut self) -> Result<Vec<Task>, DriverError> {
        let order = self
            .planner
            .as_ref()
            .expect("a planner")
            .ordered_jobs(&self.roots);
        let mut tasks = Vec::new();
        for job in order {
            if !self.needrun.get(&job).copied().unwrap_or(false) || self.tasks.contains_key(&job) {
                continue;
            }
            let planner = self.planner.as_ref().expect("a planner");
            let entry = &planner.jobs[job];
            if entry.incomplete {
                continue;
            }
            let waiting = entry.dependencies.iter().any(|(_, dependency)| {
                self.needrun.get(dependency).copied().unwrap_or(false)
                    && !self.done.contains(dependency)
            });
            if waiting {
                continue;
            }
            let id = TaskId(self.next_task);
            self.next_task += 1;
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

    pub fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        self.evaluate()?;
        self.plan()?;
        self.report_assumed();
        self.ready_tasks()
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
        for file in &outcome.files {
            let path = self.interpreter.host.resolve(&file.path);
            self.files.add_output(
                &path,
                Produced {
                    location: file.location.clone(),
                    directory: file.directory,
                },
            );
        }
        self.done.insert(job);
        let planner = self.planner.as_ref().expect("a planner");
        if planner.rules[planner.jobs[job].rule].checkpoint {
            self.replan = true;
        }
        if self.replan && self.outstanding == 0 {
            self.replan = false;
            self.plan()?;
        }
        self.ready_tasks()
    }

    pub fn finish(&mut self) -> Result<(), DriverError> {
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

enum Request {
    Start,
    Complete(Box<Outcome>),
    Finish,
}

type Reply = Result<Vec<Task>, DriverError>;

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

/// The driver of a Snakemake workflow: `launch.target` is a Snakefile, or a folder with a
/// `Snakefile` or `workflow/Snakefile`, which is then the working directory.
pub fn driver(launch: &Launch) -> Result<Box<dyn Driver>, DriverError> {
    let (requests, incoming) = channel::<Request>();
    let (outgoing, replies) = channel::<Reply>();
    let launch = launch.clone();
    let worker = std::thread::Builder::new()
        .name("snakemake".into())
        .stack_size(512 << 20)
        .spawn(move || {
            let mut session = match Session::new(&launch) {
                Ok(session) => Some(session),
                Err(error) => {
                    // Every request gets the error of the setup.
                    while let Ok(request) = incoming.recv() {
                        let finish = matches!(request, Request::Finish);
                        let _ = outgoing.send(Err(error.clone()));
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
            while let Ok(request) = incoming.recv() {
                let reply = match request {
                    Request::Start => session.start(),
                    Request::Complete(outcome) => session.complete(*outcome),
                    Request::Finish => {
                        let result = session.finish().map(|()| Vec::new());
                        let _ = outgoing.send(result);
                        break;
                    }
                };
                if outgoing.send(reply).is_err() {
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
