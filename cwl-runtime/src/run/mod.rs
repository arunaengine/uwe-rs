//! The state of a run: every process instance (tool, expression tool, workflow, scatter) as a
//! job in an arena, with the parent it reports to. Steps start as soon as all their sources
//! have values, so steps that wait for outputs of earlier tasks start from `complete`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use cwl_parser::iri;
use cwl_parser::model::{Class, Process, short_name};
use workflow_graph::{Files, Note, Outcome, Task, TaskId};

use crate::expression::Variables;
use crate::files;
use crate::inputs::{self, Setting};
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
    Task(Box<Pending>),
    Workflow(Box<WorkflowRun>),
    Scatter(ScatterRun),
    Finished,
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
}

enum Event {
    Finished(JobId, Map),
    Step {
        workflow: JobId,
        step: usize,
        outputs: Map,
    },
}

/// A run of one document.
pub struct Run {
    options: Options,
    jobs: Vec<Job>,
    tasks: HashMap<TaskId, JobId>,
    next_task: u64,
    ready: Vec<Task>,
    events: VecDeque<Event>,
    result: Option<Map>,
    /// The folder URL of placeholder files, in a run on placeholder inputs.
    placeholders: Option<String>,
    /// Location prefixes of files whose content is not real: the work folders of tasks in a
    /// simulated run, and the placeholder folder.
    fake: Vec<String>,
    /// Whether the documents name ontologies in `$schemas`.
    ontologies: bool,
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
    pub fn new(options: Options) -> Self {
        Run {
            options,
            jobs: Vec::new(),
            tasks: HashMap::new(),
            next_task: 1,
            ready: Vec::new(),
            events: VecDeque::new(),
            result: None,
            placeholders: None,
            fake: Vec::new(),
            ontologies: false,
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

    /// The markers found since the last call.
    pub fn take_notes(&mut self) -> Vec<Note> {
        std::mem::take(&mut self.notes)
    }

    /// Notes that the documents name ontologies in `$schemas`, which iunctus does not read, so
    /// that a file format may be a subclass of a declared one.
    pub fn name_ontologies(&mut self, named: bool) {
        self.ontologies = named;
    }

    /// Runs on placeholder inputs, whose files are named in the folder `base`.
    pub fn use_placeholders(&mut self, base: &str) {
        self.placeholders = Some(base.to_owned());
        self.fake.push(base.to_owned());
        self.assumed.push(base.to_owned());
    }

    /// Starts the main process with its input object and returns the first tasks.
    pub fn start(
        &mut self,
        process: &Arc<Process>,
        job: Map,
        name: &str,
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
        self.launch(process, job, &Scope::default(), Parent::Root, names, true)?;
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
        let outputs = pending.collect(outcome, self.files.as_deref())?;
        self.events.push_back(Event::Finished(job, outputs));
        self.settle()?;
        Ok(std::mem::take(&mut self.ready))
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
        toplevel: bool,
    ) -> Result<(), String> {
        let id = self.jobs.len();
        self.jobs.push(Job {
            parent,
            state: State::Finished,
        });
        let first = self.start_process(id, process, job.clone(), outer, &names, toplevel);
        let leaf = !matches!(process.class, Class::Workflow(_));
        match (first, &self.placeholders) {
            // In a run on placeholder inputs, optional inputs are null, but the expressions
            // of a tool may need them: the optional inputs that a failing expression names get
            // placeholders, a few rounds at most.
            (Err(error), Some(base)) if leaf && !error.contains(tool::UNSUPPORTED) => {
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
                        Err(next) => (job, last) = (filled, next),
                    }
                }
                // Errors that name no input, like a library function that reads one: all
                // optional inputs get placeholders.
                match placeholder::fill_optional(process, &job, outer, &base, None) {
                    Some(filled) => self
                        .start_process(id, process, filled, outer, &names, toplevel)
                        .map_err(|_| error),
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
            .check_supported()
            .map_err(|error| format!("{}{}", tool::UNSUPPORTED, error.message))?;
        let evaluator = scope.evaluator();
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
            ontologies: self.ontologies,
            assumed: &self.assumed,
            placeholder_strings: &self.placeholder_strings,
        };
        let job = inputs::prepare(process, job, &setting).map_err(at)?;
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
                    Err(_) if fake => Value::Map(Map::new()),
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
        while let Some(event) = self.events.pop_front() {
            match event {
                Event::Finished(job, outputs) => self.deliver(job, outputs)?,
                Event::Step {
                    workflow,
                    step,
                    outputs,
                } => self.step_done(workflow, step, outputs)?,
            }
        }
        Ok(())
    }

    fn deliver(&mut self, job: JobId, outputs: Map) -> Result<(), String> {
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
