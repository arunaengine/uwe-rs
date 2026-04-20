//! The state of a run: every process instance (tool, expression tool, workflow, scatter) as a
//! job in an arena, with the parent it reports to. Steps start as soon as all their sources
//! have values, so steps that wait for outputs of earlier tasks start from `complete`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use cwl_parser::iri;
use cwl_parser::model::{
    Class, LinkMerge, PickValue, Process, ScatterMethod, Step, StepInput, short_name,
};
use workflow_graph::{Outcome, Task, TaskId};

use crate::expression::{Evaluator, Variables};
use crate::files;
use crate::inputs::{self, Setting};
use crate::placeholder;
use crate::scope::Scope;
use crate::tool::{self, OUTDIR, Options, Pending, TMPDIR};
use crate::value::{Map, Value};

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
    /// mock run, and the placeholder folder.
    fake: Vec<String>,
    /// Whether the documents name ontologies in `$schemas`.
    ontologies: bool,
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
        }
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
        if self.options.mock
            && let Some(folder) = tool::workdir(outcome)
        {
            let prefix = format!("{}/", iri::file_url(std::path::Path::new(&folder)));
            if !self.fake.contains(&prefix) {
                self.fake.push(prefix);
            }
        }
        let outputs = pending.collect(outcome)?;
        self.events.push_back(Event::Finished(job, outputs));
        self.settle()?;
        Ok(std::mem::take(&mut self.ready))
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
            discover: toplevel || self.options.mock,
            placeholders: self.placeholders.as_deref(),
            toplevel,
            fill_optional: self.options.fill_optional,
            ontologies: self.ontologies,
        };
        let job = inputs::prepare(process, job, &setting).map_err(at)?;
        match &process.class {
            Class::CommandLineTool(_) | Class::Operation => {
                let task_id = TaskId(self.next_task);
                self.next_task += 1;
                let fake =
                    self.options.mock && placeholder::is_fake(&Value::Map(job.clone()), &self.fake);
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
                self.jobs[id].state = State::Task(Box::new(pending));
            }
            Class::ExpressionTool(tool) => {
                let runtime = expression_runtime();
                let inputs = Value::Map(job);
                // Over files whose content is not real, an expression may fail or give values
                // of the wrong type; its outputs are then placeholders.
                let fake = self.options.mock && placeholder::is_fake(&inputs, &self.fake);
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
                    Err(error) => return Err(at(error)),
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
                    let value = if fake && !types.matches(&output.value_type, &value) {
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

    fn start_step(
        &mut self,
        workflow: JobId,
        index: usize,
        step: &Step,
        gathered: Map,
        scope: &Scope,
        names: Names,
    ) -> Result<(), String> {
        let at = |error: String| format!("{}: {error}", names.display);
        let step_scope = scope.with(&step.requirements, &step.hints);
        let evaluator = step_scope.evaluator();
        let process = &step.run.process;
        if step.scatter.is_empty() {
            match self
                .step_job(step, gathered, &evaluator, &step_scope)
                .map_err(at)?
            {
                Some(job) => {
                    let parent = Parent::Step {
                        workflow,
                        step: index,
                    };
                    self.launch(process, job, &step_scope, parent, names, false)?;
                }
                None => {
                    let outputs = null_outputs(step);
                    self.events.push_back(Event::Step {
                        workflow,
                        step: index,
                        outputs,
                    });
                }
            }
            return Ok(());
        }
        // Scatter: one job per element of the scattered inputs.
        let keys: Vec<String> = step
            .scatter
            .iter()
            .map(|id| short_name(id).to_owned())
            .collect();
        let mut gathered = gathered;
        if let Some(base) = &self.placeholders {
            // Optional inputs are null in a run on placeholder inputs; a scatter over one gets
            // a list of one placeholder.
            for key in &keys {
                if gathered.get(key).is_none_or(Value::is_null) {
                    let item = process
                        .inputs
                        .iter()
                        .find(|input| short_name(&input.id) == key)
                        .map(|input| {
                            let mut types = step_scope.types();
                            types.add(&input.value_type);
                            let mut value = placeholder::input(input, &types, base);
                            let _ = files::normalize(&mut value);
                            value
                        })
                        .unwrap_or_else(|| Value::string(key.as_str()));
                    gathered.insert(key.clone(), Value::List(vec![item]));
                }
            }
        }
        let mut lengths = Vec::new();
        for key in &keys {
            match gathered.get(key) {
                Some(Value::List(items)) => lengths.push(items.len()),
                Some(Value::Null) | None => {
                    return Err(at(format!(
                        "the scattered input `{key}` is null, not a list"
                    )));
                }
                Some(other) => {
                    return Err(at(format!(
                        "the scattered input `{key}` is not a list: {}",
                        other.to_json()
                    )));
                }
            }
        }
        let method = step.scatter_method.unwrap_or(ScatterMethod::Dotproduct);
        let combinations: Vec<Vec<usize>> = match method {
            ScatterMethod::Dotproduct if self.placeholders.is_some() => {
                // Placeholder lists have one item, where the document may expect as many as
                // another list has: shorter lists repeat their items.
                let longest = lengths.iter().copied().max().unwrap_or(0);
                let shortest = lengths.iter().copied().min().unwrap_or(0);
                let count = if shortest == 0 { 0 } else { longest };
                (0..count)
                    .map(|index| lengths.iter().map(|length| index % length).collect())
                    .collect()
            }
            ScatterMethod::Dotproduct => {
                if lengths.iter().any(|length| *length != lengths[0]) {
                    return Err(at("dotproduct scatter needs lists of equal length".into()));
                }
                (0..lengths[0])
                    .map(|index| vec![index; keys.len()])
                    .collect()
            }
            ScatterMethod::NestedCrossproduct | ScatterMethod::FlatCrossproduct => {
                let mut combinations = vec![Vec::new()];
                for length in &lengths {
                    let mut next = Vec::new();
                    for combination in &combinations {
                        for index in 0..*length {
                            let mut extended = combination.clone();
                            extended.push(index);
                            next.push(extended);
                        }
                    }
                    combinations = next;
                }
                combinations
            }
        };
        let shape = match method {
            ScatterMethod::NestedCrossproduct => lengths.clone(),
            _ => vec![combinations.len()],
        };
        let scatter = self.jobs.len();
        self.jobs.push(Job {
            parent: Parent::Step {
                workflow,
                step: index,
            },
            state: State::Scatter(ScatterRun {
                outputs: step
                    .outputs
                    .iter()
                    .map(|id| short_name(id).to_owned())
                    .collect(),
                shape,
                results: vec![None; combinations.len()],
                remaining: combinations.len(),
            }),
        });
        for (element, combination) in combinations.iter().enumerate() {
            let mut inputs = gathered.clone();
            for (key, item) in keys.iter().zip(combination) {
                let value = match gathered.get(key) {
                    Some(Value::List(items)) => items[*item].clone(),
                    _ => Value::Null,
                };
                inputs.insert(key.clone(), value);
            }
            let shown = if method == ScatterMethod::Dotproduct {
                vec![element]
            } else {
                combination.clone()
            };
            let element_names = names.indexed(&shown);
            let job = self
                .step_job(step, inputs, &evaluator, &step_scope)
                .map_err(|error| format!("{}: {error}", element_names.display))?;
            match job {
                Some(job) => {
                    let parent = Parent::Element {
                        scatter,
                        index: element,
                    };
                    self.launch(process, job, &step_scope, parent, element_names, false)?;
                }
                None => {
                    let State::Scatter(run) = &mut self.jobs[scatter].state else {
                        unreachable!("the scatter was just made");
                    };
                    run.results[element] = Some(Map::new());
                    run.remaining -= 1;
                }
            }
        }
        if let State::Scatter(run) = &mut self.jobs[scatter].state
            && run.remaining == 0
        {
            let assembled = assemble(run);
            self.events.push_back(Event::Finished(scatter, assembled));
        }
        Ok(())
    }

    /// [`Run::element`], and on placeholder inputs, with placeholders for the null inputs
    /// that a failing `valueFrom` or `when` names, a few rounds at most.
    fn step_job(
        &self,
        step: &Step,
        gathered: Map,
        evaluator: &Evaluator,
        scope: &Scope,
    ) -> Result<Option<Map>, String> {
        let first = self.element(step, gathered.clone(), evaluator);
        let (Err(error), Some(base)) = (&first, &self.placeholders) else {
            return first;
        };
        let mut values = gathered;
        let mut last = error.clone();
        for _ in 0..8 {
            let mut changed = false;
            for input in &step.run.process.inputs {
                let name = short_name(&input.id);
                if values.get(name).is_some_and(|value| !value.is_null())
                    || !placeholder::names(&last, name)
                {
                    continue;
                }
                let mut types = scope.types();
                types.add(&input.value_type);
                let mut value = placeholder::input(input, &types, base);
                files::normalize(&mut value)?;
                values.insert(name.to_owned(), value);
                changed = true;
            }
            if !changed {
                break;
            }
            match self.element(step, values.clone(), evaluator) {
                Ok(job) => return Ok(job),
                Err(next) => last = next,
            }
        }
        // `valueFrom` runs before `when`, so a `valueFrom` over optional inputs that `when`
        // guards fails; other runners check `when` first, and so does this fallback.
        if let Some(when) = &step.when {
            let inputs = Value::Map(values);
            let skipped = evaluator.evaluate(
                when,
                Variables {
                    inputs: &inputs,
                    this: &Value::Null,
                    runtime: &expression_runtime(),
                },
            );
            if matches!(skipped, Ok(Value::Bool(false) | Value::Null)) {
                return Ok(None);
            }
        }
        first
    }

    /// The input object of one step job: `valueFrom` after the sources, then `when`; `None`
    /// when `when` skips it. Only inputs of the process it runs are passed on.
    fn element(
        &self,
        step: &Step,
        gathered: Map,
        evaluator: &Evaluator,
    ) -> Result<Option<Map>, String> {
        let access = self.options.access();
        let mut sources = gathered;
        for input in &step.inputs {
            if input.load_contents != Some(true) {
                continue;
            }
            if let Some(Value::Map(file)) = sources.get_mut(short_name(&input.id))
                && file.get("class").and_then(Value::as_str) == Some("File")
            {
                access.load_contents(file)?;
            }
        }
        let before = Value::Map(sources.clone());
        let runtime = expression_runtime();
        let mut job = sources;
        for input in &step.inputs {
            let Some(expression) = &input.value_from else {
                continue;
            };
            let name = short_name(&input.id);
            let mut this = before.get(name).cloned().unwrap_or(Value::Null);
            access.load_listings(&mut this, true);
            let value = evaluator.evaluate(
                expression,
                Variables {
                    inputs: &before,
                    this: &this,
                    runtime: &runtime,
                },
            )?;
            job.insert(name.to_owned(), value);
        }
        if let Some(when) = &step.when {
            let inputs = Value::Map(job.clone());
            let value = evaluator.evaluate(
                when,
                Variables {
                    inputs: &inputs,
                    this: &Value::Null,
                    runtime: &runtime,
                },
            )?;
            match value {
                Value::Bool(true) => {}
                Value::Bool(false) => return Ok(None),
                // Conditions on optional inputs, which are null on placeholder inputs.
                Value::Null if self.placeholders.is_some() => return Ok(None),
                other => {
                    return Err(format!(
                        "`when` must give true or false, not {}",
                        other.to_json()
                    ));
                }
            }
        }
        let accepted: HashSet<&str> = step
            .run
            .process
            .inputs
            .iter()
            .map(|input| short_name(&input.id))
            .collect();
        job.retain(|name, _| accepted.contains(name.as_str()));
        Ok(Some(job))
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

/// Fails when a source or output source names nothing in the workflow.
fn check_sources(process: &Process, workflow: &cwl_parser::model::Workflow) -> Result<(), String> {
    let mut known: HashSet<&str> = process
        .inputs
        .iter()
        .map(|input| input.id.as_str())
        .collect();
    for step in &workflow.steps {
        known.extend(step.outputs.iter().map(String::as_str));
    }
    let sources = workflow
        .steps
        .iter()
        .flat_map(|step| step.inputs.iter().flat_map(|input| &input.source))
        .chain(
            process
                .outputs
                .iter()
                .flat_map(|output| &output.output_source),
        );
    for source in sources {
        if !known.contains(source.as_str()) {
            return Err(format!(
                "the source `{}` names no input or step output",
                short_name(source)
            ));
        }
    }
    Ok(())
}

/// The input object of a step from its sources, as cwltool's `object_from_state`: sources
/// merge by `linkMerge`, `pickValue` picks, and `default` stands in for null. `None` while a
/// source has no value yet. `lenient` takes a failed pick as null, for runs on placeholder
/// inputs, where optional inputs are null and conditions follow placeholder values.
fn gather(
    values: &HashMap<String, Value>,
    step: &Step,
    lenient: bool,
) -> Option<Result<Map, String>> {
    let mut map = Map::new();
    for input in &step.inputs {
        let mut sources = Vec::new();
        for source in &input.source {
            sources.push(values.get(source)?.clone());
        }
        let value =
            merge(input, sources).and_then(|value| pick(input.pick_value, value, &input.id));
        let value = match value {
            Ok(value) => value,
            Err(_) if lenient => Value::Null,
            Err(error) => return Some(Err(error)),
        };
        let value = match (&value, &input.default) {
            (Value::Null, Some(default)) => {
                let mut value = Value::from_node(default);
                if let Err(error) = files::normalize(&mut value) {
                    return Some(Err(error));
                }
                value
            }
            _ => value,
        };
        map.insert(short_name(&input.id).to_owned(), value);
    }
    Some(Ok(map))
}

fn merge(input: &StepInput, mut sources: Vec<Value>) -> Result<Value, String> {
    let link_merge = input
        .link_merge
        .or((sources.len() > 1).then_some(LinkMerge::MergeNested));
    Ok(match link_merge {
        None => sources.pop().unwrap_or(Value::Null),
        Some(LinkMerge::MergeNested) => Value::List(sources),
        Some(LinkMerge::MergeFlattened) => {
            let mut items = Vec::new();
            for source in sources {
                match source {
                    Value::List(inner) => items.extend(inner),
                    other => items.push(other),
                }
            }
            Value::List(items)
        }
    })
}

/// Applies `pickValue` to a list of values.
fn pick(pick: Option<PickValue>, value: Value, id: &str) -> Result<Value, String> {
    let (Some(pick), Value::List(items)) = (pick, &value) else {
        return Ok(value);
    };
    let present: Vec<&Value> = items.iter().filter(|item| !item.is_null()).collect();
    let name = short_name(id);
    match pick {
        PickValue::FirstNonNull => present
            .first()
            .map(|value| (*value).clone())
            .ok_or_else(|| format!("all sources of `{name}` are null")),
        PickValue::OnlyNonNull => match present.as_slice() {
            [only] => Ok((*only).clone()),
            [] => Err(format!("all sources of `{name}` are null")),
            _ => Err(format!("more than one source of `{name}` is not null")),
        },
        PickValue::AllNonNull => Ok(Value::List(present.into_iter().cloned().collect())),
    }
}

/// The outputs of a skipped step: null for every output.
fn null_outputs(step: &Step) -> Map {
    step.outputs
        .iter()
        .map(|id| (short_name(id).to_owned(), Value::Null))
        .collect()
}

/// The output object of a workflow from its output sources.
/// `lenient` takes a failed pick as null, as for steps on placeholder inputs.
fn workflow_outputs(run: &WorkflowRun, lenient: bool) -> Result<Map, String> {
    let mut outputs = Map::new();
    for output in &run.process.outputs {
        let sources: Vec<Value> = output
            .output_source
            .iter()
            .map(|source| run.values.get(source).cloned().unwrap_or(Value::Null))
            .collect();
        let link_merge = output
            .link_merge
            .or((sources.len() > 1).then_some(LinkMerge::MergeNested));
        let mut sources = sources;
        let value = match link_merge {
            None => sources.pop().unwrap_or(Value::Null),
            Some(LinkMerge::MergeNested) => Value::List(sources),
            Some(LinkMerge::MergeFlattened) => {
                let mut items = Vec::new();
                for source in sources {
                    match source {
                        Value::List(inner) => items.extend(inner),
                        other => items.push(other),
                    }
                }
                Value::List(items)
            }
        };
        let value = match pick(output.pick_value, value, &output.id) {
            Err(_) if lenient => Value::Null,
            result => result?,
        };
        outputs.insert(short_name(&output.id).to_owned(), value);
    }
    Ok(outputs)
}

/// The outputs of a scatter: a list per output, nested for nested_crossproduct; skipped
/// elements give null.
fn assemble(run: &ScatterRun) -> Map {
    let mut outputs = Map::new();
    for name in &run.outputs {
        let flat: Vec<Value> = run
            .results
            .iter()
            .map(|result| {
                result
                    .as_ref()
                    .and_then(|map| map.get(name).cloned())
                    .unwrap_or(Value::Null)
            })
            .collect();
        outputs.insert(name.clone(), nest(&flat, &run.shape));
    }
    outputs
}

/// Folds a flat list into nested lists of the given lengths, the last varying fastest.
fn nest(flat: &[Value], shape: &[usize]) -> Value {
    match shape {
        [] | [_] => Value::List(flat.to_vec()),
        [first, rest @ ..] => {
            let size: usize = rest.iter().product();
            Value::List(
                (0..*first)
                    .map(|index| nest(&flat[index * size..(index + 1) * size], rest))
                    .collect(),
            )
        }
    }
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
