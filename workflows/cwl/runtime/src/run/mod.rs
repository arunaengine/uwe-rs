//! Tracks process instances and advances workflow jobs when their sources are ready.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet, VecDeque};
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cwl_parser::iri;
use cwl_parser::model::{
    Class, Process, Requirement, RequirementClass, ScatterMethod, Step, short_name,
};
use workflow_graph::{Files, Note, Outcome, Task, TaskId};

use crate::expression::Variables;
use crate::files;
use crate::inputs::{self, Setting};
use crate::ontology::Ontology;
use crate::placeholder;
use crate::scope::Scope;
use crate::tool::{self, OUTDIR, Options, Pending, TMPDIR};
use crate::value::{Map, Value};

mod links;
mod marks;
mod step;

use links::{assemble, check_sources, gather, workflow_outputs};

type JobId = usize;

/// Where a job reports its outputs.
#[derive(Clone, Copy, Debug)]
enum Parent {
    Root,
    Step { workflow: JobId, step: usize },
    Element { scatter: JobId, index: usize },
}

#[derive(Debug)]
enum State {
    Deferred(Box<Deferred>),
    Task(Box<Pending>),
    Workflow(Box<WorkflowRun>),
    Scatter(ScatterRun),
    Finished,
}

#[derive(Debug)]
struct Deferred {
    process: Arc<Process>,
    job: Map,
    outer: Scope,
    names: Names,
    toplevel: bool,
    step: Option<Arc<Step>>,
}

#[derive(Debug)]
struct Job {
    parent: Parent,
    state: State,
}

/// The names of a job: for display, with scatter indexes, and the path of its step.
#[derive(Clone, Debug, Default)]
struct Names {
    display: String,
    step: String,
}

impl Names {
    fn child(&self, step: &str) -> Names {
        let join = |prefix: &str| {
            if prefix.is_empty() {
                step.to_owned()
            } else {
                format!("{prefix}/{step}")
            }
        };
        Names {
            display: join(&self.display),
            step: join(&self.step),
        }
    }

    fn indexed(&self, index: &[usize]) -> Names {
        let index: Vec<String> = index.iter().map(usize::to_string).collect();
        Names {
            display: format!("{}[{}]", self.display, index.join(",")),
            step: self.step.clone(),
        }
    }
}

#[derive(Debug)]
struct WorkflowRun {
    process: Arc<Process>,
    scope: Scope,
    names: Names,
    /// Values by identifier: the workflow inputs and the outputs of finished steps.
    values: HashMap<String, Value>,
    started: Vec<bool>,
    finished: Vec<bool>,
    done: bool,
}

#[derive(Debug)]
struct ScatterRun {
    outputs: Vec<String>,
    /// The length of each scattered input for nested_crossproduct, else one dimension.
    shape: Vec<usize>,
    results: Vec<Option<Map>>,
    remaining: usize,
    cursor: Option<Box<ScatterCursor>>,
}

#[derive(Debug)]
struct ScatterCursor {
    step: Arc<Step>,
    scope: Scope,
    names: Names,
    common: Map,
    keys: Vec<String>,
    columns: Vec<Vec<Value>>,
    lengths: Vec<usize>,
    count: usize,
    next: usize,
    method: ScatterMethod,
}

enum Event {
    Finished(JobId, Map),
}

fn directory_sources(value: &Value, member: bool, sources: &mut HashSet<(String, bool)>) {
    match value {
        Value::Map(map) => {
            let directory = value.text("class") == Some("Directory");
            let member = member || directory;
            if member
                && value.is_file_like()
                && let Some(location) = value.text("location")
            {
                sources.insert((location.to_owned(), directory));
            }
            for value in map.values() {
                directory_sources(value, member, sources);
            }
        }
        Value::List(items) => {
            for value in items {
                directory_sources(value, member, sources);
            }
        }
        _ => {}
    }
}

fn source_conflicts(
    location: &str,
    directory: bool,
    writer: &str,
    remaining: &mut usize,
    deadline: Instant,
) -> Result<bool, String> {
    if location == writer
        || (directory && writer.starts_with(&format!("{}/", location.trim_end_matches('/'))))
    {
        return Ok(true);
    }
    let (Some(path), Some(writer)) = (files::local_path(location), files::local_path(writer))
    else {
        return Ok(false);
    };
    if directory {
        let (Ok(path), Ok(writer_path)) = (path.canonicalize(), writer.canonicalize()) else {
            return Ok(false);
        };
        if writer_path.starts_with(&path) {
            return Ok(true);
        }
        let Ok(identity) = writer.metadata() else {
            return Ok(false);
        };
        if identity.nlink() <= 1 {
            return Ok(false);
        }
        let unresolved = |reason: String| {
            format!(
                "{}cannot resolve in-place File aliases in Directory {location}: {reason}",
                tool::UNSUPPORTED
            )
        };
        let mut pending = vec![path];
        let mut visited = HashSet::new();
        while let Some(path) = pending.pop() {
            let canonical = path
                .canonicalize()
                .map_err(|error| unresolved(error.to_string()))?;
            if !visited.insert(canonical) {
                continue;
            }
            let entries = std::fs::read_dir(path).map_err(|error| unresolved(error.to_string()))?;
            for entry in entries {
                if *remaining == 0 || Instant::now() >= deadline {
                    return Err(unresolved("bounded directory scan exhausted".into()));
                }
                *remaining -= 1;
                let entry = entry.map_err(|error| unresolved(error.to_string()))?;
                let found = entry
                    .metadata()
                    .map_err(|error| unresolved(error.to_string()))?;
                if found.dev() == identity.dev() && found.ino() == identity.ino() {
                    return Ok(true);
                }
                if found.is_dir() {
                    pending.push(entry.path());
                }
            }
        }
        Ok(false)
    } else {
        let (Ok(path), Ok(writer)) = (path.metadata(), writer.metadata()) else {
            return Ok(false);
        };
        Ok(path.dev() == writer.dev() && path.ino() == writer.ino())
    }
}

/// A run of one document.
pub struct Run {
    options: Options,
    keep_going: bool,
    stopped: bool,
    jobs: Vec<Job>,
    tasks: HashMap<TaskId, JobId>,
    next_task: u64,
    ready: Vec<Task>,
    deferred: VecDeque<JobId>,
    writer: Option<JobId>,
    in_place_sources: HashSet<String>,
    events: VecDeque<Event>,
    result: Option<Map>,
    /// The folder URL of placeholder files, in a run on placeholder inputs.
    placeholders: Option<String>,
    /// Location prefixes of files whose content is not real: the work folders of tasks in a
    /// simulated run, and the placeholder folder.
    fake: Vec<String>,
    ontologies: Ontology,
    /// Location prefixes of files that have what a parameter asks of them: the placeholder
    /// folder, and the work folders of abstract `Operation` tasks.
    assumed: Vec<String>,
    /// The strings that the placeholders of the main process hold.
    placeholder_strings: HashSet<String>,
    /// The task of each job of a tool, also after it finished.
    task_of_job: HashMap<JobId, TaskId>,
    /// The markers found since the engine last asked, for the graph.
    notes: Vec<Note>,
    /// The files of the tasks, where a real executor keeps them.
    files: Option<Arc<dyn Files>>,
}

impl Run {
    pub fn new(options: Options, keep_going: bool) -> Self {
        Run {
            options,
            keep_going,
            stopped: false,
            jobs: Vec::new(),
            tasks: HashMap::new(),
            next_task: 1,
            ready: Vec::new(),
            deferred: VecDeque::new(),
            writer: None,
            in_place_sources: HashSet::new(),
            events: VecDeque::new(),
            result: None,
            placeholders: None,
            fake: Vec::new(),
            ontologies: Ontology::default(),
            assumed: Vec::new(),
            placeholder_strings: HashSet::new(),
            task_of_job: HashMap::new(),
            notes: Vec::new(),
            files: None,
        }
    }

    /// Reads the content of task outputs through these files, as a store elsewhere needs.
    pub fn use_files(&mut self, files: Arc<dyn Files>) {
        self.files = Some(files);
    }

    pub fn use_cancel(&mut self, cancel: workflow_graph::CancellationToken) {
        self.options.javascript.bind_cancel(cancel);
    }

    /// The markers found since the last call.
    pub fn take_notes(&mut self) -> Vec<Note> {
        std::mem::take(&mut self.notes)
    }

    /// Loads format relationships only when an exact format does not match.
    pub fn name_ontologies(&mut self, schemas: Vec<String>) {
        self.ontologies = Ontology::new(schemas, self.options.remote.clone());
    }

    /// Runs on placeholder inputs, whose files are named in the folder `base`.
    pub fn use_placeholders(&mut self, base: &str) {
        self.placeholders = Some(base.to_owned());
        self.fake.push(base.to_owned());
        self.assumed.push(base.to_owned());
    }

    /// Starts the main process with its input object and returns the first tasks. The
    /// requirements of the input object override those of every process.
    pub fn start(
        &mut self,
        process: &Arc<Process>,
        job: Map,
        name: &str,
        requirements: Vec<Requirement>,
    ) -> Result<Vec<Task>, String> {
        let names = Names {
            display: name.to_owned(),
            step: name.to_owned(),
        };
        if let Some(base) = &self.placeholders {
            let mut types = Scope::default().enter(process).types();
            for input in &process.inputs {
                types.add(&input.value_type);
            }
            self.placeholder_strings = placeholder::strings(process, &types, base);
        }
        let scope = Scope::overriding(requirements);
        self.launch(process, job, &scope, Parent::Root, names, None)?;
        self.settle()?;
        Ok(std::mem::take(&mut self.ready))
    }

    /// Takes the outcome of a task and returns the tasks that can run now.
    pub fn complete(&mut self, outcome: &Outcome) -> Result<Vec<Task>, String> {
        let Some(job) = self.tasks.remove(&outcome.id) else {
            return Err(format!("no task {} is waiting", outcome.id));
        };
        let state = std::mem::replace(&mut self.jobs[job].state, State::Finished);
        let State::Task(pending) = state else {
            return Err(format!("task {} belongs to no command", outcome.id));
        };
        if self.writer == Some(job) {
            self.writer = None;
        }
        if self.options.simulated
            && let Some(folder) = tool::workdir(outcome)
        {
            let prefix = format!("{}/", iri::file_url(std::path::Path::new(&folder)));
            if pending.is_abstract() && !self.assumed.contains(&prefix) {
                self.assumed.push(prefix.clone());
            }
            if !self.fake.contains(&prefix) {
                self.fake.push(prefix);
            }
        }
        let updates = pending.updated(outcome);
        let outputs = match pending.collect(outcome, self.files.as_deref()) {
            Ok(outputs) => outputs,
            Err(error) => {
                if !self.keep_going {
                    self.stopped = true;
                }
                if self.keep_going
                    && let Err(later) = self.settle()
                {
                    return Err(format!(
                        "{error}; while preparing independent work: {later}"
                    ));
                }
                return Err(error);
            }
        };
        self.events.push_back(Event::Finished(job, outputs));
        for (old, new) in &updates {
            self.update_in_place(old, new);
        }
        self.settle()?;
        Ok(std::mem::take(&mut self.ready))
    }

    /// Propagates updated file or directory versions and drops stale listings, sizes and
    /// checksums.
    fn update_in_place(&mut self, old: &str, new: &str) {
        let inside = format!("{old}/");
        let path = files::local_path(old);
        let identity = path.as_ref().and_then(|path| path.metadata().ok());
        let canonical = path.and_then(|path| path.canonicalize().ok());
        let mut update = |value: &mut Value| {
            files::visit_files(value, &mut |file| {
                let location = file.get("location").and_then(Value::as_str);
                let moved = match location {
                    Some(location) if location == old => new.to_owned(),
                    Some(location) if location.starts_with(&inside) => {
                        format!("{new}/{}", &location[inside.len()..])
                    }
                    Some(location) => {
                        let Some(path) = files::local_path(location) else {
                            return;
                        };
                        let Some(identity) = &identity else {
                            return;
                        };
                        if path.metadata().is_ok_and(|found| {
                            found.dev() == identity.dev() && found.ino() == identity.ino()
                        }) {
                            new.to_owned()
                        } else if identity.is_dir() {
                            let (Some(original), Ok(path)) = (&canonical, path.canonicalize())
                            else {
                                return;
                            };
                            let Ok(relative) = path.strip_prefix(original) else {
                                return;
                            };
                            let suffix = iri::file_url(relative);
                            format!("{new}/{}", suffix.trim_start_matches("file://"))
                        } else {
                            return;
                        }
                    }
                    None => return,
                };
                file.insert("location".into(), Value::String(moved));
                for stale in ["listing", "size", "checksum", "contents"] {
                    file.shift_remove(stale);
                }
            });
        };
        for job in &mut self.jobs {
            match &mut job.state {
                State::Deferred(raw) => raw.job.values_mut().for_each(&mut update),
                State::Workflow(run) => run.values.values_mut().for_each(&mut update),
                State::Scatter(scatter) => {
                    if let Some(cursor) = &mut scatter.cursor {
                        cursor.common.values_mut().for_each(&mut update);
                        cursor.columns.iter_mut().flatten().for_each(&mut update);
                    }
                    for result in scatter.results.iter_mut().flatten() {
                        result.values_mut().for_each(&mut update);
                    }
                }
                State::Task(_) | State::Finished => {}
            }
        }
        for event in &mut self.events {
            let Event::Finished(_, outputs) = event;
            outputs.values_mut().for_each(&mut update);
        }
    }

    /// The tasks that can run now, which a call that failed may have left behind.
    pub fn take_ready(&mut self) -> Vec<Task> {
        std::mem::take(&mut self.ready)
    }

    /// The output object of the main process, once it finished.
    pub fn outputs(&self) -> Option<&Map> {
        self.result.as_ref()
    }

    /// Why the run has not finished: the steps that wait for inputs.
    pub fn waiting(&self) -> Vec<String> {
        let mut waiting = Vec::new();
        for job in &self.jobs {
            if let State::Workflow(run) = &job.state {
                let Class::Workflow(workflow) = &run.process.class else {
                    continue;
                };
                for (index, step) in workflow.steps.iter().enumerate() {
                    if !run.started[index] {
                        waiting.push(run.names.child(short_name(&step.id)).display);
                    }
                }
            }
        }
        waiting
    }

    /// Starts a process for an input object; its outputs go to `parent`.
    fn launch(
        &mut self,
        process: &Arc<Process>,
        job: Map,
        outer: &Scope,
        parent: Parent,
        names: Names,
        step: Option<Arc<Step>>,
    ) -> Result<(), String> {
        let id = self.jobs.len();
        self.jobs.push(Job {
            parent,
            state: State::Deferred(Box::new(Deferred {
                process: Arc::clone(process),
                job,
                outer: outer.clone(),
                names,
                toplevel: matches!(parent, Parent::Root),
                step,
            })),
        });
        self.deferred.push_back(id);
        Ok(())
    }

    fn start_deferred(&mut self, id: JobId) -> Result<(), String> {
        let state = std::mem::replace(&mut self.jobs[id].state, State::Finished);
        let State::Deferred(raw) = state else {
            return Err("a deferred job lost its inputs".into());
        };
        let Deferred {
            process,
            mut job,
            outer,
            names,
            toplevel,
            step,
        } = *raw;
        if let Some(step) = step {
            job = match self
                .step_job(
                    &step,
                    job,
                    &outer.evaluator(&self.options.javascript),
                    &outer,
                )
                .map_err(|error| format!("{}: {error}", names.display))?
            {
                Some(job) => job,
                None => {
                    self.events
                        .push_back(Event::Finished(id, links::null_outputs(&step)));
                    return Ok(());
                }
            };
        }
        let process = &process;
        let outer = &outer;
        let first = self.start_process(id, process, job.clone(), outer, &names, toplevel);
        let leaf = !matches!(process.class, Class::Workflow(_));
        match (first, &self.placeholders) {
            // In a run on placeholder inputs, optional inputs are null, but the expressions
            // of a tool may need them: the optional inputs that a failing expression names get
            // placeholders, a few rounds at most.
            (Err(error), Some(base))
                if leaf
                    && !error.contains(tool::UNSUPPORTED)
                    && !crate::javascript::helper::boundary(&error) =>
            {
                let base = base.clone();
                let mut job = job;
                let mut last = error.clone();
                for _ in 0..8 {
                    let Some(filled) =
                        placeholder::fill_optional(process, &job, outer, &base, Some(&last))
                    else {
                        break;
                    };
                    match self.start_process(id, process, filled.clone(), outer, &names, toplevel) {
                        Ok(()) => return Ok(()),
                        Err(next) if crate::javascript::helper::boundary(&next) => {
                            return Err(next);
                        }
                        Err(next) => (job, last) = (filled, next),
                    }
                }
                // Errors that name no input, like a library function that reads one: all
                // optional inputs get placeholders.
                match placeholder::fill_optional(process, &job, outer, &base, None) {
                    Some(filled) => self
                        .start_process(id, process, filled, outer, &names, toplevel)
                        .map_err(|later| {
                            if crate::javascript::helper::boundary(&later) {
                                later
                            } else {
                                error
                            }
                        }),
                    None => Err(error),
                }
            }
            (result, _) => result,
        }
    }

    /// Prepares the input object of a job and starts its process.
    fn start_process(
        &mut self,
        id: JobId,
        process: &Arc<Process>,
        job: Map,
        outer: &Scope,
        names: &Names,
        toplevel: bool,
    ) -> Result<(), String> {
        let at = |error: String| {
            if names.display.is_empty() {
                error
            } else {
                format!("{}: {error}", names.display)
            }
        };
        let scope = outer.enter(process);
        scope
            .check_supported(self.options.software)
            .map_err(|error| format!("{}{}", tool::UNSUPPORTED, error.message))?;
        let evaluator = scope.evaluator(&self.options.javascript);
        let mut types = scope.types();
        for input in &process.inputs {
            types.add(&input.value_type);
        }
        let setting = Setting {
            types: &types,
            evaluator: &evaluator,
            access: self.options.access(),
            load_listing: scope.load_listing(),
            discover: toplevel,
            placeholders: self.placeholders.as_deref(),
            toplevel,
            fill_optional: self.options.fill_optional,
            ontologies: &self.ontologies,
            assumed: &self.assumed,
            placeholder_strings: &self.placeholder_strings,
        };
        let job = inputs::prepare(process, job, &setting).map_err(at)?;
        let mut job_directories = HashSet::new();
        for value in job.values() {
            directory_sources(value, false, &mut job_directories);
        }
        self.check_directories(&job_directories).map_err(at)?;
        match &process.class {
            Class::CommandLineTool(_) | Class::Operation => {
                let task_id = TaskId(self.next_task);
                self.next_task += 1;
                let fake = self.options.simulated
                    && placeholder::is_fake(&Value::Map(job.clone()), &self.fake);
                let (task, pending) = tool::task(
                    process,
                    job,
                    &scope,
                    &self.options,
                    task_id,
                    (&names.display, &names.step),
                    fake,
                )
                .map_err(at)?;
                let mut staged_directories = HashSet::new();
                for directory in task.inputs.iter().filter(|input| input.directory) {
                    for input in task.inputs.iter().filter(|input| {
                        input.path == directory.path
                            || input.path.starts_with(&format!("{}/", directory.path))
                    }) {
                        if !input.source.is_empty() {
                            let source = if iri::has_scheme(&input.source) {
                                input.source.clone()
                            } else {
                                iri::file_url(std::path::Path::new(&input.source))
                            };
                            staged_directories.insert((source, input.directory));
                        }
                    }
                }
                self.check_directories(&staged_directories).map_err(at)?;
                if pending.in_place_sources().next().is_some() {
                    job_directories.extend(staged_directories);
                    job_directories.extend(self.held_directories());
                    let mut remaining = 10_000;
                    let deadline = Instant::now() + Duration::from_millis(100);
                    for writer in pending.in_place_sources() {
                        for (location, directory) in &job_directories {
                            if source_conflicts(
                                location,
                                *directory,
                                writer,
                                &mut remaining,
                                deadline,
                            )
                            .map_err(at)?
                            {
                                return Err(at(format!(
                                    "{}in-place File updates cannot be combined with their parent Directory views",
                                    tool::UNSUPPORTED
                                )));
                            }
                        }
                    }
                }
                self.in_place_sources
                    .extend(pending.in_place_sources().map(str::to_owned));
                self.ready.push(task);
                self.tasks.insert(task_id, id);
                self.task_of_job.insert(id, task_id);
                self.jobs[id].state = State::Task(Box::new(pending));
            }
            Class::ExpressionTool(tool) => {
                let runtime = expression_runtime();
                let inputs = Value::Map(job);
                // Over files whose content is not real, an expression may fail or give values
                // of the wrong type; its outputs are then placeholders.
                let fake = self.options.simulated && placeholder::is_fake(&inputs, &self.fake);
                let value = evaluator.evaluate(
                    &tool.expression,
                    Variables {
                        inputs: &inputs,
                        this: &Value::Null,
                        runtime: &runtime,
                    },
                );
                let value = match value {
                    Ok(Value::Map(map)) => Value::Map(map),
                    Ok(Value::Null) => Value::Map(Map::new()),
                    Err(error) if fake && !crate::javascript::helper::boundary(&error) => {
                        Value::Map(Map::new())
                    }
                    Err(error) => return Err(at(format!("the expression: {error}"))),
                    Ok(other) if fake => {
                        let _ = other;
                        Value::Map(Map::new())
                    }
                    Ok(other) => {
                        return Err(at(format!(
                            "the expression must give an object, not {}",
                            other.to_json()
                        )));
                    }
                };
                let mut value = value;
                files::normalize(&mut value).map_err(at)?;
                let Value::Map(mut outputs) = value else {
                    unreachable!("an object stays a map")
                };
                for output in &process.outputs {
                    types.add(&output.value_type);
                }
                let base = self
                    .placeholders
                    .clone()
                    .unwrap_or_else(|| format!("_:{}/", names.step));
                for output in &process.outputs {
                    let name = short_name(&output.id).to_owned();
                    let value = outputs.get(&name).cloned().unwrap_or(Value::Null);
                    // Over files without real content, a null output is what real data
                    // would fill, so it gets a placeholder too.
                    let unfit = value.is_null() || !types.matches(&output.value_type, &value);
                    if !self.options.simulated && !types.matches(&output.value_type, &value) {
                        return Err(at(format!(
                            "the output `{name}` does not match its declared type: {}",
                            value.summary()
                        )));
                    }
                    let value = if fake && unfit {
                        let hint = placeholder::Hint::default();
                        let mut value =
                            placeholder::value(&types, &output.value_type, &name, &base, &hint);
                        files::normalize(&mut value).map_err(at)?;
                        value
                    } else {
                        value
                    };
                    outputs.insert(name, value);
                }
                self.events.push_back(Event::Finished(id, outputs));
            }
            Class::Workflow(workflow) => {
                let mut values = HashMap::new();
                for input in &process.inputs {
                    let value = job
                        .get(short_name(&input.id))
                        .cloned()
                        .unwrap_or(Value::Null);
                    values.insert(input.id.clone(), value);
                }
                check_sources(process, workflow).map_err(at)?;
                let steps = workflow.steps.len();
                let run = WorkflowRun {
                    process: Arc::clone(process),
                    scope,
                    names: if toplevel {
                        Names::default()
                    } else {
                        names.clone()
                    },
                    values,
                    started: vec![false; steps],
                    finished: vec![false; steps],
                    done: false,
                };
                self.jobs[id].state = State::Workflow(Box::new(run));
                self.advance(id)?;
            }
        }
        Ok(())
    }

    /// Handles finished jobs until none is left; this may start more jobs.
    fn settle(&mut self) -> Result<(), String> {
        let mut failure = None;
        let mut failures = 0;
        'admission: loop {
            while let Some(Event::Finished(job, outputs)) = self.events.pop_front() {
                if let Err(error) = self.deliver(job, outputs) {
                    failure.get_or_insert(error);
                    failures += 1;
                    if !self.keep_going {
                        self.stopped = true;
                        break 'admission;
                    }
                }
            }
            if self.stopped || self.writer.is_some() {
                break;
            }
            let Some(&id) = self.deferred.front() else {
                break;
            };
            // A cursor keeps declaration order without allocating every scattered job.
            if matches!(self.jobs[id].state, State::Scatter(_)) {
                if self.tasks.len() >= 64 {
                    break;
                }
                let descendant = |mut job: JobId| loop {
                    match self.jobs[job].parent {
                        Parent::Root => return false,
                        Parent::Step { workflow, .. } => job = workflow,
                        Parent::Element { scatter, .. } => {
                            if scatter == id {
                                return true;
                            }
                            job = scatter;
                        }
                    }
                };
                let pending = self
                    .deferred
                    .iter()
                    .filter(|job| descendant(**job))
                    .take(64)
                    .count();
                if pending >= 64 {
                    let position = self
                        .deferred
                        .iter()
                        .position(|job| descendant(*job))
                        .expect("deferred job");
                    let next = self.deferred.remove(position).expect("deferred job");
                    self.deferred.push_front(next);
                } else {
                    self.deferred.pop_front();
                    self.scatter_next(id)?;
                    let next = self.deferred.pop_back().expect("scatter element");
                    self.deferred.push_front(next);
                }
                continue;
            }
            let State::Deferred(raw) = &self.jobs[id].state else {
                return Err("a deferred job lost its inputs".into());
            };
            let writer = matches!(raw.process.class, Class::CommandLineTool(_))
                && raw
                    .outer
                    .enter(&raw.process)
                    .find(|class| match class {
                        RequirementClass::InplaceUpdate { inplace_update } => Some(*inplace_update),
                        _ => None,
                    })
                    .is_some_and(|(enabled, _)| enabled);
            if writer && !self.tasks.is_empty() {
                break;
            }
            self.deferred.pop_front();
            if let Err(error) = self.start_deferred(id) {
                failure.get_or_insert(error);
                failures += 1;
                if !self.keep_going {
                    self.stopped = true;
                    break;
                }
                continue;
            }
            if writer && matches!(self.jobs[id].state, State::Task(_)) {
                self.writer = Some(id);
            }
        }
        match failure {
            Some(error) if failures > 1 => Err(format!(
                "{error}; and {} more deferred jobs failed",
                failures - 1
            )),
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn check_directories(&self, sources: &HashSet<(String, bool)>) -> Result<(), String> {
        let mut remaining = 10_000;
        let deadline = Instant::now() + Duration::from_millis(100);
        for (location, directory) in sources {
            for writer in &self.in_place_sources {
                if source_conflicts(location, *directory, writer, &mut remaining, deadline)? {
                    return Err(format!(
                        "{}parent Directory views cannot follow in-place File updates",
                        tool::UNSUPPORTED
                    ));
                }
            }
        }
        Ok(())
    }

    fn held_directories(&self) -> HashSet<(String, bool)> {
        let mut sources = HashSet::new();
        let mut gather = |value: &Value| directory_sources(value, false, &mut sources);
        for job in &self.jobs {
            match &job.state {
                State::Deferred(raw) => raw.job.values().for_each(&mut gather),
                State::Workflow(run) => run.values.values().for_each(&mut gather),
                State::Scatter(scatter) => {
                    if let Some(cursor) = &scatter.cursor {
                        cursor.common.values().for_each(&mut gather);
                        cursor.columns.iter().flatten().for_each(&mut gather);
                    }
                    for result in scatter.results.iter().flatten() {
                        result.values().for_each(&mut gather);
                    }
                }
                State::Task(_) | State::Finished => {}
            }
        }
        for event in &self.events {
            let Event::Finished(_, outputs) = event;
            outputs.values().for_each(&mut gather);
        }
        sources
    }

    fn observe_directories(&self, value: &Value) -> Result<(), String> {
        let mut sources = HashSet::new();
        directory_sources(value, false, &mut sources);
        self.check_directories(&sources)
    }

    fn deliver(&mut self, job: JobId, outputs: Map) -> Result<(), String> {
        for value in outputs.values() {
            self.observe_directories(value)?;
        }
        let parent = self.jobs[job].parent;
        self.jobs[job].state = State::Finished;
        match parent {
            Parent::Root => {
                let mut value = Value::Map(outputs);
                let access = self.options.access();
                let mut error = None;
                files::visit_files(&mut value, &mut |file| {
                    let is_file = file.get("class").and_then(Value::as_str) == Some("File");
                    if is_file
                        && !file.contains_key("checksum")
                        && let Err(message) = access.add_checksum(file)
                    {
                        error.get_or_insert(message);
                    }
                    file.shift_remove("path");
                });
                if let Some(message) = error {
                    return Err(message);
                }
                let Value::Map(outputs) = value else {
                    unreachable!("an object stays a map")
                };
                self.result = Some(outputs);
                Ok(())
            }
            Parent::Step { workflow, step } => self.step_done(workflow, step, outputs),
            Parent::Element { scatter, index } => {
                let State::Scatter(run) = &mut self.jobs[scatter].state else {
                    return Err("a scatter element lost its scatter".into());
                };
                run.results[index] = Some(outputs);
                run.remaining -= 1;
                if run.remaining == 0 {
                    let assembled = assemble(run);
                    self.events.push_back(Event::Finished(scatter, assembled));
                }
                Ok(())
            }
        }
    }

    /// Records the outputs of a step and starts the steps that can run now.
    fn step_done(&mut self, workflow: JobId, step: usize, outputs: Map) -> Result<(), String> {
        let State::Workflow(run) = &mut self.jobs[workflow].state else {
            return Err("a step lost its workflow".into());
        };
        let Class::Workflow(model) = &run.process.class else {
            return Err("a step lost its workflow".into());
        };
        for output in &model.steps[step].outputs {
            let value = outputs
                .get(short_name(output))
                .cloned()
                .unwrap_or(Value::Null);
            run.values.insert(output.clone(), value);
        }
        run.finished[step] = true;
        self.advance(workflow)
    }

    /// Starts every step of a workflow whose sources all have values, and ends the workflow
    /// when all steps finished.
    fn advance(&mut self, workflow: JobId) -> Result<(), String> {
        loop {
            let State::Workflow(run) = &mut self.jobs[workflow].state else {
                return Ok(());
            };
            let process = Arc::clone(&run.process);
            let Class::Workflow(model) = &process.class else {
                return Ok(());
            };
            let lenient = self.placeholders.is_some();
            let next = model.steps.iter().enumerate().find_map(|(index, step)| {
                if run.started[index] {
                    return None;
                }
                gather(&run.values, step, lenient).map(|inputs| (index, inputs))
            });
            let Some((index, gathered)) = next else {
                break;
            };
            run.started[index] = true;
            let scope = run.scope.clone();
            let names = run.names.clone();
            let step = &model.steps[index];
            let step_names = names.child(short_name(&step.id));
            self.start_step(workflow, index, step, gathered?, &scope, step_names)?;
        }
        let State::Workflow(run) = &mut self.jobs[workflow].state else {
            return Ok(());
        };
        if run.done || run.finished.iter().any(|finished| !finished) {
            return Ok(());
        }
        run.done = true;
        let lenient = self.placeholders.is_some();
        let outputs = workflow_outputs(run, lenient).map_err(|error| {
            if run.names.display.is_empty() {
                error
            } else {
                format!("{}: {error}", run.names.display)
            }
        })?;
        self.events.push_back(Event::Finished(workflow, outputs));
        Ok(())
    }
}

/// The `runtime` of expressions outside command line tools.
fn expression_runtime() -> Value {
    let mut runtime = Map::new();
    runtime.insert("outdir".into(), Value::string(OUTDIR));
    runtime.insert("tmpdir".into(), Value::string(TMPDIR));
    runtime.insert("cores".into(), Value::Integer(1));
    runtime.insert("ram".into(), Value::Integer(256));
    runtime.insert("outdirSize".into(), Value::Integer(1024));
    runtime.insert("tmpdirSize".into(), Value::Integer(1024));
    Value::Map(runtime)
}

#[cfg(test)]
mod tests;
