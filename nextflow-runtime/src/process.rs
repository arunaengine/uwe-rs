//! Processes in the network: each round of input values becomes a task, and each task outcome
//! becomes output values (channels spec, section 7).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, RenderedOutput, RenderedPart, TaskDirective};
use nextflow_evaluator::lowered::{Input, Module, Process};
use nextflow_evaluator::units::{parse_duration, parse_memory};
use nextflow_evaluator::value::{ChannelId, Key, PathValue, Value};

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
    /// A process whose inputs are all values runs one task.
    pub singleton: bool,
    pub stub: bool,
    pub rounds: u32,
    /// Whether an input closed, so no more rounds start.
    pub poisoned: bool,
    pub pending: HashMap<TaskId, Vec<RenderedOutput>>,
}

impl Behaviour for ProcessNode {
    fn ignite(&mut self, context: &mut Context) -> Result<State, Error> {
        self.run(context)
    }

    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        while !self.poisoned && !(self.singleton && self.rounds == 1) {
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
            self.start_task(context, values)?;
        }
        self.finish_if_done(context)
    }

    fn complete(&mut self, context: &mut Context, outcome: TaskOutcome) -> Result<State, Error> {
        let Some(outputs) = self.pending.remove(&outcome.id) else {
            return Ok(State::Running);
        };
        if outcome.exit_status != 0 {
            let message = format!(
                "Process `{}` terminated with an error exit status ({})",
                self.name, outcome.exit_status
            );
            return Err(thrown(Exception::new("ProcessFailedException", message)));
        }
        // All outputs of a task bind together, in declaration order.
        // `eval` results come in the order of the `eval` parts over all outputs.
        let mut values = Vec::new();
        let mut eval_index = 0;
        for output in &outputs {
            values.push(self.output_value(output, &outcome, &mut eval_index)?);
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
    /// Binds the inputs of one task, evaluates its directives and `task` object, renders it,
    /// and asks for it to run unless `when:` skips it.
    fn start_task(&mut self, context: &mut Context, values: Vec<Value>) -> Result<(), Error> {
        let index = self.rounds;
        let interpreter = &mut *context.interpreter;
        let mut frame = interpreter.bind_task(&self.module, &self.process, &values)?;
        let base = task_object(&self.name, index, &[]);
        let directives = interpreter.directives(&mut frame, &self.process, &base)?;
        let task = task_object(&self.name, index, &directives);
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
                RenderedPart::Path { pattern, .. } => request.output_patterns.push(pattern.clone()),
                RenderedPart::Env(name) => request.env.push(name.clone()),
                RenderedPart::Eval(command) => request.eval.push(command.clone()),
                RenderedPart::Value(_) | RenderedPart::Stdout => {}
            }
        }
        self.pending.insert(id, rendered.outputs);
        context.request(request);
        Ok(())
    }

    /// The value one output binds for a task; `None` when an optional output is missing.
    fn output_value(
        &self,
        output: &RenderedOutput,
        outcome: &TaskOutcome,
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
                RenderedPart::Path { pattern, options } => {
                    let files: Vec<Value> = outcome
                        .files
                        .iter()
                        .filter(|(name, _)| glob_matches(pattern, name))
                        .map(|(_, path)| Value::Path(Rc::new(path.clone())))
                        .collect();
                    let arity = option(options, "arity");
                    if files.is_empty() && !output.optional {
                        let message = format!(
                            "Missing output file(s) `{pattern}` expected by process `{}`",
                            self.name
                        );
                        return Err(thrown(Exception::new("MissingFileException", message)));
                    }
                    match (files.as_slice(), arity) {
                        ([single], None) => single.clone(),
                        _ => Value::list(files),
                    }
                }
            };
            parts.push(value);
        }
        let value = if output.tuple {
            Value::list(parts.clone())
        } else {
            parts.remove(0)
        };
        let missing = output.optional && parts.iter().any(|part| !part.truth());
        Ok(if missing { None } else { Some(value) })
    }

    /// Closes the outputs once no round can start and no task is in flight.
    fn finish_if_done(&mut self, context: &mut Context) -> Result<State, Error> {
        let done = self.poisoned || (self.singleton && self.rounds == 1);
        if done && self.pending.is_empty() {
            for output in self.outputs.clone() {
                context.close(output);
            }
            return Ok(State::Stopped);
        }
        Ok(State::Running)
    }
}

/// The `task` object of a task: the directives' values, with Nextflow's defaults.
fn task_object(process: &str, index: u32, directives: &[TaskDirective]) -> Value {
    let mut entries: Vec<(Value, Value)> = vec![
        (Value::string("process"), Value::string(process)),
        (Value::string("index"), Value::integer(index as i32)),
        (Value::string("attempt"), Value::integer(1)),
        (Value::string("cpus"), Value::integer(1)),
        (Value::string("ext"), Value::map([])),
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

/// A glob pattern with `*`, `?` and `**` against a relative file name.
pub(crate) fn glob_matches(pattern: &str, name: &str) -> bool {
    fn matches(pattern: &[char], name: &[char]) -> bool {
        match (pattern.first(), name.first()) {
            (None, None) => true,
            (Some('*'), _) if pattern.get(1) == Some(&'*') => {
                (0..=name.len()).any(|skip| matches(&pattern[2..], &name[skip..]))
            }
            (Some('*'), _) => (0..=name.len())
                .take_while(|skip| *skip == 0 || name[skip - 1] != '/')
                .any(|skip| matches(&pattern[1..], &name[skip..])),
            (Some('?'), Some(c)) if *c != '/' => matches(&pattern[1..], &name[1..]),
            (Some(p), Some(c)) if p == c => matches(&pattern[1..], &name[1..]),
            _ => false,
        }
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    matches(&pattern, &name)
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
