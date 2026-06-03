//! Processes in the network: each round of input values becomes a task, and each task outcome
//! becomes output values (channels spec, section 7).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::rc::Rc;

use nextflow_config::ProcessSettings;
use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{
    Error, Interpreter, RenderedOutput, RenderedPart, TaskDirective, TaskFrame,
};
use nextflow_evaluator::lowered::{Input, Module, Process};
use nextflow_evaluator::units::{parse_duration, parse_memory};
use nextflow_evaluator::value::{ChannelId, Key, MapKind, PathValue, Value};

use crate::channel::Message;
use crate::network::{Behaviour, Context, State};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskId(pub u64);

/// A task that a process asks to run.
#[derive(Debug)]
pub struct TaskRequest {
    pub id: TaskId,
    /// The fully qualified process name, as `task.process` shows it.
    pub process: Rc<str>,
    /// The display name, like `GUNZIP (sample.fastq.gz)`.
    pub name: String,
    pub script: String,
    pub directives: Vec<TaskDirective>,
    /// Each input file and the name it is staged under.
    pub staged: Vec<(PathValue, String)>,
    /// The glob patterns of the output files, relative to the task directory.
    pub output_patterns: Vec<String>,
    /// The names of the `env` outputs.
    pub env: Vec<Rc<str>>,
    /// The commands of the `eval` outputs, run after the script.
    pub eval: Vec<String>,
}

/// What a finished task produced.
#[derive(Debug, Default)]
pub struct TaskOutcome {
    pub id: TaskId,
    pub exit_status: i32,
    /// The output files: their name relative to the task directory, and where they are now.
    pub files: Vec<(String, PathValue)>,
    pub stdout: String,
    pub env: HashMap<String, String>,
    /// The output of each `eval` command, in the order of the request.
    pub eval: Vec<String>,
}

pub(crate) struct ProcessNode {
    pub module: Rc<Module>,
    pub process: Rc<Process>,
    pub name: Rc<str>,
    pub outputs: Vec<ChannelId>,
    /// A process whose inputs are all values, without `each` inputs, runs one task.
    pub singleton: bool,
    /// Whether every input is a value channel, so that one round of values starts tasks.
    pub one_round: bool,
    pub stub: bool,
    /// The config settings of this call of the process.
    pub settings: Rc<ProcessSettings>,
    pub rounds: u32,
    /// The number of tasks started, which gives each task its index.
    pub tasks: u32,
    /// Whether an input closed, so no more rounds start.
    pub poisoned: bool,
    /// The tasks in flight: the name of each, and its outputs.
    pub pending: HashMap<TaskId, (String, Vec<RenderedOutput>)>,
}

impl Behaviour for ProcessNode {
    fn ignite(&mut self, context: &mut Context) -> Result<State, Error> {
        self.run(context)
    }

    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        while !self.poisoned && !(self.one_round && self.rounds == 1) {
            // A round starts when every input has a message.
            let mut messages = Vec::new();
            for input in 0..context.input_count() {
                match context.peek(input) {
                    Some(message) => messages.push(message),
                    None => return Ok(State::Running),
                }
            }
            if messages
                .iter()
                .any(|message| matches!(message, Message::Stop))
            {
                self.poisoned = true;
                break;
            }
            for input in 0..context.input_count() {
                if !context.is_value(input) {
                    context.consume(input);
                }
            }
            let values = messages
                .into_iter()
                .map(|message| match message {
                    Message::Value(value) => value,
                    Message::Stop => Value::Null,
                })
                .collect();
            self.rounds += 1;
            self.start_tasks(context, values)?;
        }
        self.finish_if_done(context)
    }

    fn complete(&mut self, context: &mut Context, outcome: TaskOutcome) -> Result<State, Error> {
        let Some((task, outputs)) = self.pending.remove(&outcome.id) else {
            return Ok(State::Running);
        };
        if outcome.exit_status != 0 {
            let message = format!(
                "Process `{task}` terminated with an error exit status ({})",
                outcome.exit_status
            );
            return Err(thrown(Exception::new("ProcessFailedException", message)));
        }
        // All outputs of a task bind together, in declaration order.
        // `eval` results come in the order of the `eval` parts over all outputs.
        let mut values = Vec::new();
        let mut eval_index = 0;
        for output in &outputs {
            values.push(self.output_value(output, &outcome, &task, &mut eval_index)?);
        }
        for (channel, value) in self.outputs.clone().into_iter().zip(values) {
            if let Some(value) = value {
                context.emit(channel, value);
            }
        }
        self.finish_if_done(context)
    }
}

impl ProcessNode {
    /// Starts the tasks of one round of input values: one task, or with `each` inputs one
    /// task for each combination of their values, the first varying fastest, as Nextflow's
    /// `ForwardClosure` does.
    fn start_tasks(&mut self, context: &mut Context, values: Vec<Value>) -> Result<(), Error> {
        let each: Vec<usize> = (self.process.inputs.iter().enumerate())
            .filter(|(_, input)| matches!(input, Input::Each(_)))
            .map(|(index, _)| index)
            .collect();
        if each.is_empty() {
            return self.start_task(context, values);
        }
        let lists: Vec<Value> = each.iter().map(|index| values[*index].clone()).collect();
        for combination in nextflow_evaluator::interpreter::combinations(&lists) {
            let mut values = values.clone();
            for (index, value) in each.iter().zip(combination) {
                values[*index] = value;
            }
            self.start_task(context, values)?;
        }
        Ok(())
    }

    /// Binds the inputs of one task, evaluates its directives and `task` object, renders it,
    /// and asks for it to run unless `when:` skips it.
    fn start_task(&mut self, context: &mut Context, values: Vec<Value>) -> Result<(), Error> {
        self.tasks += 1;
        let index = self.tasks;
        let interpreter = &mut *context.interpreter;
        let mut frame =
            interpreter.bind_task_as(&self.module, &self.process, &self.name, &values)?;
        let ext = ext_map(&self.settings);
        let base = task_object(&self.name, index, &[], &ext);
        let mut directives = interpreter.directives(&mut frame, &self.process, &base)?;
        // Selectors replace the process's own directives; plain settings fill in the rest.
        for (name, value) in &self.settings.selected {
            if &**name != "ext" {
                let value = setting(interpreter, &frame, name, value, &base)?;
                directives.retain(|directive| directive.name != *name);
                directives.extend(value);
            }
        }
        for (name, value) in &self.settings.defaults {
            let current = directives.iter().find(|directive| directive.name == *name);
            if &**name != "ext" && current.is_none_or(built_in_default) {
                let value = setting(interpreter, &frame, name, value, &base)?;
                directives.retain(|directive| directive.name != *name);
                directives.extend(value);
            }
        }
        limit_resources(&mut directives);
        let task = task_object(&self.name, index, &directives, &ext);
        let rendered = interpreter.render(&mut frame, &self.process, &task, self.stub)?;
        if !rendered.run {
            return Ok(());
        }
        let id = context.task_id();
        let mut request = TaskRequest {
            id,
            process: self.name.clone(),
            name: display_name(&self.name, &directives, index, self.singleton),
            script: rendered.script,
            directives,
            staged: frame.staged.clone(),
            output_patterns: Vec::new(),
            env: Vec::new(),
            eval: Vec::new(),
        };
        for part in rendered.outputs.iter().flat_map(|output| &output.parts) {
            match part {
                RenderedPart::Path { patterns, .. } => {
                    request.output_patterns.extend(patterns.iter().cloned())
                }
                RenderedPart::Env(name) => request.env.push(name.clone()),
                RenderedPart::Eval(command) => request.eval.push(command.clone()),
                RenderedPart::Value(_) | RenderedPart::Stdout => {}
            }
        }
        self.pending
            .insert(id, (request.name.clone(), rendered.outputs));
        context.request(request);
        Ok(())
    }

    /// The value one output binds for a task; `None` when an optional output is missing.
    fn output_value(
        &self,
        output: &RenderedOutput,
        outcome: &TaskOutcome,
        task: &str,
        eval_index: &mut usize,
    ) -> Result<Option<Value>, Error> {
        let mut parts = Vec::new();
        for part in &output.parts {
            let value = match part {
                RenderedPart::Value(value) => value.clone(),
                RenderedPart::Stdout => Value::string(outcome.stdout.as_str()),
                RenderedPart::Env(name) => outcome
                    .env
                    .get(&**name)
                    .map_or(Value::Null, |v| Value::string(v.as_str())),
                RenderedPart::Eval(_) => {
                    let value = outcome
                        .eval
                        .get(*eval_index)
                        .map_or(Value::Null, |v| Value::string(v.as_str()));
                    *eval_index += 1;
                    value
                }
                RenderedPart::Path { patterns, options } => {
                    let mut files: Vec<Value> = Vec::new();
                    for pattern in patterns {
                        let found: Vec<Value> = outcome
                            .files
                            .iter()
                            .filter(|(name, _)| glob_matches(pattern, name))
                            .map(|(_, path)| Value::Path(Rc::new(path.clone())))
                            .collect();
                        if found.is_empty() && !output.optional {
                            let message = format!(
                                "Missing output file(s) `{pattern}` expected by process `{task}`"
                            );
                            return Err(thrown(Exception::new("MissingFileException", message)));
                        }
                        files.extend(found);
                    }
                    let arity = option(options, "arity");
                    match (files.as_slice(), arity) {
                        ([single], None) => single.clone(),
                        _ => Value::list(files),
                    }
                }
            };
            parts.push(value);
        }
        // An optional output is missing when a part of it is, like a pattern without files.
        let missing = output.optional && parts.iter().any(|part| !part.truth());
        let value = if output.tuple {
            Value::list(parts)
        } else {
            parts.remove(0)
        };
        Ok(if missing { None } else { Some(value) })
    }

    /// Closes the outputs once no round can start and no task is in flight.
    fn finish_if_done(&mut self, context: &mut Context) -> Result<State, Error> {
        let done = self.poisoned || (self.one_round && self.rounds == 1);
        if done && self.pending.is_empty() {
            for output in self.outputs.clone() {
                context.close(output);
            }
            return Ok(State::Stopped);
        }
        Ok(State::Running)
    }
}

/// The `task.ext` map of the config: selectors replace entries, plain settings add missing
/// ones. Closures in it are called when a script reads them.
fn ext_map(settings: &ProcessSettings) -> Value {
    let ext = Value::map([]);
    let Value::Map(map) = &ext else {
        unreachable!("a map")
    };
    map.borrow_mut().kind = MapKind::Lazy;
    let selected = settings.selected.iter().map(|setting| (setting, true));
    let defaults = settings.defaults.iter().map(|setting| (setting, false));
    for ((name, value), replace) in selected.chain(defaults) {
        let Value::Map(entries) = value else {
            continue;
        };
        if &**name != "ext" {
            continue;
        }
        for (key, value) in &entries.borrow().entries {
            let mut map = map.borrow_mut();
            if replace || !map.entries.contains_key(key) {
                map.entries.insert(key.clone(), value.clone());
            }
        }
    }
    ext
}

/// The directive of a config setting for a task. A `tag` whose closure fails is left out, as
/// Nextflow's `TaskRun.getName` ignores the error and names the task by its index.
fn setting(
    interpreter: &mut Interpreter,
    frame: &TaskFrame,
    name: &Rc<str>,
    value: &Value,
    task_object: &Value,
) -> Result<Option<TaskDirective>, Error> {
    match interpreter.resolve_setting(frame, value, task_object) {
        Ok(value) => Ok(Some(TaskDirective {
            name: name.clone(),
            arguments: vec![value],
        })),
        Err(error) if &**name == "tag" && error.exception().is_some() => Ok(None),
        Err(error) => Err(error),
    }
}

/// Whether a directive has the value Nextflow gives it when nothing sets it, which a plain
/// config setting replaces.
fn built_in_default(directive: &TaskDirective) -> bool {
    let [value] = directive.arguments.as_slice() else {
        return false;
    };
    match (&*directive.name, value) {
        ("debug", Value::Boolean(value)) => !value,
        ("cacheable", Value::Boolean(value)) => *value,
        ("maxRetries", Value::Number(value)) => value.to_f64() == 1.0,
        ("maxErrors", Value::Number(value)) => value.to_f64() == -1.0,
        ("errorStrategy", Value::String(text)) => text.eq_ignore_ascii_case("terminate"),
        ("shell", Value::List(list)) => {
            let items: Vec<String> = list.borrow().items.iter().map(Value::to_string).collect();
            items == ["/bin/bash", "-ue"]
        }
        _ => false,
    }
}

/// The `task` object of a task: the directives' values, with Nextflow's defaults.
fn task_object(process: &str, index: u32, directives: &[TaskDirective], ext: &Value) -> Value {
    let mut entries: Vec<(Value, Value)> = vec![
        (Value::string("process"), Value::string(process)),
        (Value::string("index"), Value::integer(index as i32)),
        (Value::string("attempt"), Value::integer(1)),
        (Value::string("cpus"), Value::integer(1)),
        (Value::string("ext"), ext.clone()),
        (
            Value::string("shell"),
            Value::list(vec![Value::string("/bin/bash"), Value::string("-ue")]),
        ),
    ];
    for directive in directives {
        let value = match (directive.name.as_ref(), directive.arguments.as_slice()) {
            ("memory" | "disk", [Value::String(text)]) => parse_memory(text)
                .map(Value::MemoryUnit)
                .unwrap_or(Value::Null),
            ("time", [Value::String(text)]) => parse_duration(text)
                .map(Value::Duration)
                .unwrap_or(Value::Null),
            (_, [value]) => value.clone(),
            (_, values) => Value::list(values.to_vec()),
        };
        entries.push((Value::string(&*directive.name), value));
    }
    Value::map(entries)
}

/// Reduces the `cpus`, `memory` and `time` directives to the `resourceLimits` directive.
fn limit_resources(directives: &mut [TaskDirective]) {
    let limits = directives
        .iter()
        .find(|directive| &*directive.name == "resourceLimits")
        .and_then(|directive| directive.arguments.first().cloned());
    let Some(Value::Map(limits)) = limits else {
        return;
    };
    let unit = |name: &str, value: &Value| match (name, value) {
        ("memory", Value::String(text)) => {
            parse_memory(text).map_or(Value::Null, Value::MemoryUnit)
        }
        ("time", Value::String(text)) => parse_duration(text).map_or(Value::Null, Value::Duration),
        _ => value.clone(),
    };
    for directive in directives.iter_mut() {
        let name = &*directive.name;
        let limit = limits
            .borrow()
            .entries
            .get(&Key(Value::string(name)))
            .cloned();
        let (Some(limit), [value]) = (limit, directive.arguments.as_slice()) else {
            continue;
        };
        let (limit, value) = (unit(name, &limit), unit(name, value));
        let larger = match (&value, &limit) {
            (Value::Number(value), Value::Number(limit)) => value.to_f64() > limit.to_f64(),
            (Value::MemoryUnit(value), Value::MemoryUnit(limit))
            | (Value::Duration(value), Value::Duration(limit)) => value > limit,
            _ => false,
        };
        if larger && matches!(name, "cpus" | "memory" | "time") {
            directive.arguments = vec![limit];
        }
    }
}

/// `NAME (tag)`, else `NAME (index)` for a process that runs several tasks, else `NAME`.
fn display_name(
    process: &str,
    directives: &[TaskDirective],
    index: u32,
    singleton: bool,
) -> String {
    let tag = directives
        .iter()
        .rev()
        .find(|directive| &*directive.name == "tag")
        .and_then(|directive| directive.arguments.first());
    match tag {
        Some(tag) => format!("{process} ({tag})"),
        None if !singleton => format!("{process} ({index})"),
        None => process.to_owned(),
    }
}

fn option(options: &Value, name: &str) -> Option<Value> {
    match options {
        Value::Map(map) => map.borrow().entries.get(&Key(Value::string(name))).cloned(),
        _ => None,
    }
}

/// A glob pattern with `*`, `?`, `**`, `{a,b}` and `[...]` against a relative file name.
pub(crate) fn glob_matches(pattern: &str, name: &str) -> bool {
    nextflow_evaluator::interpreter::glob::matches(pattern, name)
}

/// Whether the process runs one task: none of its inputs is a queue channel, and it has no
/// `each` input.
pub(crate) fn singleton(process: &Process, queue_inputs: bool) -> bool {
    !queue_inputs
        && !process
            .inputs
            .iter()
            .any(|input| matches!(input, Input::Each(_)))
}

fn thrown(exception: Exception) -> Error {
    Error::Thrown {
        exception: Rc::new(exception),
        span: Default::default(),
    }
}
