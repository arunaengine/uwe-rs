//! Processes in the network: each round of input values becomes a task, and each task outcome
//! becomes output values (channels spec, section 7).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use nextflow_config::ProcessSettings;
use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{
    Error, Interpreter, RenderedOutput, RenderedPart, TaskDirective, TaskFrame,
};
use nextflow_evaluator::lowered::{Input, Module, Process};
use nextflow_evaluator::units::{parse_duration, parse_memory};
use nextflow_evaluator::value::{ChannelId, Key, MapKind, PathValue, Value};
use nextflow_parser::ast::BodyKind;

use crate::channel::Message;
use crate::network::{Behaviour, Context, State};
use crate::publish;
use crate::strategy::{self, Strategy};

const HANDLED_DIRECTIVES: &[&str] = &[
    "accelerator",
    "cache",
    "container",
    "cpus",
    "debug",
    "disk",
    "echo",
    "errorStrategy",
    "ext",
    "label",
    "maxRetries",
    "memory",
    "publishDir",
    "resourceLimits",
    "shell",
    "stageInMode",
    "tag",
    "time",
];

const REFUSED_DIRECTIVES: &[&str] = &[
    "afterScript",
    "arch",
    "array",
    "beforeScript",
    "clusterOptions",
    "conda",
    "containerOptions",
    "executor",
    "fair",
    "hints",
    "machineType",
    "maxErrors",
    "maxForks",
    "maxSubmitAwait",
    "module",
    "penv",
    "pod",
    "queue",
    "resourceLabels",
    "scratch",
    "secret",
    "spack",
    "stageOutMode",
    "storeDir",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskId(pub u64);

/// A task that a process asks to run.
#[derive(Debug)]
pub struct TaskRequest {
    pub id: TaskId,
    pub attempt: u32,
    /// The fully qualified process name, as `task.process` shows it.
    pub process: Rc<str>,
    /// The display name, like `GUNZIP (sample.fastq.gz)`.
    pub name: String,
    pub script: String,
    pub directives: Vec<TaskDirective>,
    /// Each input file and the name it is staged under.
    pub staged: Vec<(PathValue, String)>,
    pub input_environment: Vec<(Rc<str>, Value)>,
    pub stdin: Option<Value>,
    /// The glob patterns of the output files, relative to the task directory.
    pub output_patterns: Vec<String>,
    /// The names of the `env` outputs.
    pub env: Vec<Rc<str>>,
    /// The commands of the `eval` outputs, run after the script.
    pub eval: Vec<String>,
    /// The files that an `exec:` section made in its `task.workDir`, by their names there. Such
    /// a task ran in the interpreter already; the engine only records it.
    pub made: Option<Vec<(String, PathValue)>>,
}

/// What a finished task produced.
#[derive(Debug, Default)]
pub struct TaskOutcome {
    pub id: TaskId,
    pub exit_status: i32,
    /// An output collection failure, handled by the process like an execution failure.
    pub failure: Option<Error>,
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
    /// Where `exec:` sections run, if they may.
    pub exec_folder: Option<Rc<Path>>,
    /// The config settings of this call of the process.
    pub settings: Rc<ProcessSettings>,
    pub conda_enabled: bool,
    pub spack_enabled: bool,
    pub rounds: u32,
    /// The number of tasks started, which gives each task its index.
    pub tasks: u32,
    /// Whether an input closed, so no more rounds start.
    pub poisoned: bool,
    /// The tasks in flight.
    pub pending: HashMap<TaskId, InFlight>,
    pub each_round: Option<EachRound>,
}

/// One round's `each` choices, with the first input varying fastest.
pub(crate) struct EachRound {
    values: Vec<Value>,
    choices: Vec<(usize, Vec<Value>, usize)>,
}

/// The output files of a finished task: the names it staged its inputs under, which globs do
/// not match, and the files that its outputs took, which `publishDir` publishes.
pub(crate) struct Bound<'a> {
    staged: Vec<&'a str>,
    files: Vec<(String, PathValue)>,
    /// Whether the run is simulated, where made-up names of outputs can be those of inputs.
    simulated: bool,
}

/// A task in flight, with what the process needs when it ends.
pub(crate) struct InFlight {
    name: String,
    /// The input values, the index and the attempt of the task, to run it again.
    values: Vec<Value>,
    index: u32,
    attempt: u32,
    outputs: Vec<RenderedOutput>,
    /// The frame and the `task` object of the task, for the closures of `publishDir`.
    frame: TaskFrame,
    task: Value,
    publish: Vec<publish::Rule>,
}

impl Behaviour for ProcessNode {
    fn ignite(&mut self, _: &mut Context) -> Result<State, Error> {
        Ok(State::Running)
    }

    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        if self.each_round.is_some() {
            self.start_each_task(context)?;
            context.again_first();
            return self.finish_if_done(context);
        }
        if !self.poisoned && !(self.one_round && self.rounds == 1) {
            // A round starts when every input has a message.
            let messages: Vec<_> = (0..context.input_count())
                .map(|input| context.peek(input))
                .collect();
            if messages
                .iter()
                .any(|message| matches!(message, Some(Message::Stop)))
            {
                self.poisoned = true;
                return self.finish_if_done(context);
            }
            let Some(messages) = messages.into_iter().collect::<Option<Vec<_>>>() else {
                return Ok(State::Running);
            };
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
            context.again_first();
        }
        self.finish_if_done(context)
    }

    fn complete(
        &mut self,
        context: &mut Context,
        mut outcome: TaskOutcome,
    ) -> Result<State, Error> {
        let Some(flight) = self.pending.remove(&outcome.id) else {
            return Ok(State::Running);
        };
        context.accept(outcome.id, false);
        if outcome.exit_status != 0 || outcome.failure.is_some() {
            let failure = if outcome.exit_status == 0 {
                outcome.failure.take()
            } else {
                None
            };
            return self.failed(context, flight, &outcome, failure);
        }
        context.remember_task_files(&outcome.files);
        let (task, outputs) = (&flight.name, &flight.outputs);
        // All outputs of a task bind together, in declaration order.
        // `eval` results come in the order of the `eval` parts over all outputs.
        let mut values = Vec::new();
        let mut eval_index = 0;
        let staged: Vec<&str> = (flight.frame.staged.iter())
            .map(|(_, name)| name.as_str())
            .collect();
        let mut bound = Bound {
            staged,
            files: Vec::new(),
            simulated: context.interpreter.host().simulated(),
        };
        for output in outputs {
            match self.output_value(output, &outcome, task, &mut eval_index, &mut bound) {
                Ok(value) => values.push(value),
                Err(error) => return self.failed(context, flight, &outcome, Some(error)),
            }
        }
        let interpreter = &mut *context.interpreter;
        let deliveries = publish::deliveries(
            interpreter,
            &flight.frame,
            &flight.task,
            &flight.publish,
            &bound.files,
        )?;
        context.deliver(deliveries);
        context.accept(outcome.id, true);
        *context.succeeded += 1;
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
        let mut choices = Vec::new();
        for index in each {
            let list = &values[index];
            crate::notes::spread(context.interpreter.host(), list);
            let items = match list {
                Value::List(list) => list.borrow().items.clone(),
                Value::Set(set) => set
                    .borrow()
                    .items
                    .iter()
                    .map(|item| item.0.clone())
                    .collect(),
                other => vec![other.clone()],
            };
            choices.push((index, items, 0));
        }
        if choices.iter().any(|(_, items, _)| items.is_empty()) {
            return Ok(());
        }
        self.each_round = Some(EachRound { values, choices });
        self.start_each_task(context)
    }

    fn start_each_task(&mut self, context: &mut Context) -> Result<(), Error> {
        let round = self.each_round.as_mut().expect("an unfinished each round");
        let mut values = round.values.clone();
        for (index, items, position) in &round.choices {
            values[*index] = items[*position].clone();
        }
        let mut done = true;
        for (_, items, position) in &mut round.choices {
            *position += 1;
            if *position < items.len() {
                done = false;
                break;
            }
            *position = 0;
        }
        if done {
            self.each_round = None;
        }
        self.start_task(context, values)
    }

    /// Binds the inputs of one task, evaluates its directives and `task` object, renders it,
    /// and asks for it to run unless `when:` skips it.
    fn start_task(&mut self, context: &mut Context, values: Vec<Value>) -> Result<(), Error> {
        self.tasks += 1;
        self.launch(context, values, self.tasks, None, 1)
    }

    /// The directives of a task: its process's own, then those of the config, where selectors
    /// replace the process's own and plain settings fill in the rest.
    fn directives(
        &self,
        interpreter: &mut Interpreter,
        frame: &mut TaskFrame,
        base: &Value,
    ) -> Result<Vec<TaskDirective>, Error> {
        let mut directives = interpreter.directives(frame, &self.process, base)?;
        for (name, value) in &self.settings.selected {
            if &**name != "ext" {
                let value = setting(interpreter, frame, name, value, base)?;
                directives.retain(|directive| directive.name != *name);
                directives.extend(value);
            }
        }
        for (name, value) in &self.settings.defaults {
            let current = directives.iter().find(|directive| directive.name == *name);
            if &**name != "ext" && current.is_none_or(built_in_default) {
                let value = setting(interpreter, frame, name, value, base)?;
                directives.retain(|directive| directive.name != *name);
                directives.extend(value);
            }
        }
        for directive in &directives {
            let active = match (directive.name.as_ref(), directive.arguments.as_slice()) {
                ("maxErrors", [value]) => !unlimited_errors(value),
                ("maxErrors", _) => true,
                ("conda", _) if !self.conda_enabled => false,
                ("spack", _) if !self.spack_enabled => false,
                ("stageOutMode", [Value::String(mode)]) if &**mode == "copy" => false,
                (name, _) if REFUSED_DIRECTIVES.contains(&name) => {
                    directive.arguments.iter().any(Value::truth)
                }
                (name, _) => !HANDLED_DIRECTIVES.contains(&name) && name != "cacheable",
            };
            if active {
                return Err(Error::Unsupported {
                    message: format!("process directive `{}`", directive.name),
                    span: self.process.span,
                });
            }
        }
        limit_resources(&mut directives);
        Ok(directives)
    }

    /// Asks for an attempt of a task: the first gets a new id, a retry keeps the id of the
    /// task and counts its attempt up.
    fn launch(
        &mut self,
        context: &mut Context,
        values: Vec<Value>,
        index: u32,
        id: Option<TaskId>,
        attempt: u32,
    ) -> Result<(), Error> {
        // An `exec:` section runs here, in a folder of its own, which it knows as
        // `task.workDir`; so it needs its id first.
        let exec = self.runs_exec();
        let (id, work_dir) = match exec {
            true => {
                let id = id.unwrap_or_else(|| context.task_id());
                (Some(id), Some(self.exec_work_dir(id, attempt)?))
            }
            false => (id, None),
        };
        let interpreter = &mut *context.interpreter;
        let mut frame =
            interpreter.bind_task_as(&self.module, &self.process, &self.name, &values)?;
        let ext = ext_map(&self.settings)?;
        let base = task_object(&self.name, index, attempt, None, &[], &ext);
        let directives = self.directives(interpreter, &mut frame, &base)?;
        let task = task_object(&self.name, index, attempt, None, &directives, &ext);
        if let (Some(work_dir), Value::Map(map)) = (&work_dir, &task) {
            let path = PathValue::parse(&work_dir.to_string_lossy(), "/");
            (map.borrow_mut().entries)
                .insert(Key(Value::string("workDir")), Value::Path(Rc::new(path)));
        }
        let rendered = interpreter.render(&mut frame, &self.process, &task, self.stub)?;
        if !rendered.run {
            return Ok(());
        }
        let publish = publish::rules(interpreter, &frame, &task, &directives)?;
        let id = id.unwrap_or_else(|| context.task_id());
        let mut request = TaskRequest {
            id,
            attempt,
            process: self.name.clone(),
            name: display_name(&self.name, &directives, index, self.singleton),
            script: rendered.script,
            directives,
            staged: frame.staged.clone(),
            input_environment: frame.input_environment.clone(),
            stdin: frame.stdin.clone(),
            output_patterns: Vec::new(),
            env: Vec::new(),
            eval: Vec::new(),
            made: match &work_dir {
                Some(folder) => Some(made_files(folder).map_err(|error| {
                    thrown(Exception::new(
                        "IOException",
                        format!("cannot list the files of {}: {error}", folder.display()),
                    ))
                })?),
                None => None,
            },
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
        let flight = InFlight {
            name: request.name.clone(),
            values,
            index,
            attempt,
            outputs: rendered.outputs,
            frame,
            task,
            publish,
        };
        self.pending.insert(id, flight);
        context.request(request);
        Ok(())
    }

    /// Whether the tasks of the process run its `exec:` section, which needs a folder to run
    /// in; without one, `exec:` sections are not supported.
    fn runs_exec(&self) -> bool {
        let exec = (self.process.body.as_ref()).is_some_and(|body| body.kind == BodyKind::Exec);
        exec && !(self.stub && self.process.stub.is_some())
    }

    /// The folder of an attempt of an `exec:` section, made empty.
    fn exec_work_dir(&self, id: TaskId, attempt: u32) -> Result<PathBuf, Error> {
        let Some(folder) = &self.exec_folder else {
            return Err(Error::Unsupported {
                message: "`exec:` sections".into(),
                span: self.process.span,
            });
        };
        let work_dir = folder.join(format!("{}-{attempt}", id.0));
        let _ = std::fs::remove_dir_all(&work_dir);
        std::fs::create_dir_all(&work_dir).map_err(|error| {
            thrown(Exception::new(
                "IOException",
                format!("cannot make the folder {}: {error}", work_dir.display()),
            ))
        })?;
        Ok(work_dir)
    }

    /// What happens after a task failed, by its error strategy, which is evaluated now, with
    /// the task's exit status: a retry of the task, going on without its outputs, or the end
    /// of the run, at once or once the running tasks have ended.
    fn failed(
        &mut self,
        context: &mut Context,
        flight: InFlight,
        outcome: &TaskOutcome,
        failure: Option<Error>,
    ) -> Result<State, Error> {
        let InFlight {
            name,
            values,
            index,
            attempt,
            mut frame,
            ..
        } = flight;
        let interpreter = &mut *context.interpreter;
        let ext = ext_map(&self.settings)?;
        let status = Some(outcome.exit_status);
        let base = task_object(&self.name, index, attempt, status, &[], &ext);
        let directives = self.directives(interpreter, &mut frame, &base)?;
        let (strategy, retries) = strategy::of(&directives);
        let message = format!(
            "Process `{name}` terminated with an error exit status ({})",
            outcome.exit_status
        );
        let message = failure.as_ref().map_or(message, Error::to_string);
        let error = failure
            .unwrap_or_else(|| thrown(Exception::new("ProcessFailedException", message.clone())));
        let failure = message;
        if strategy != Strategy::Ignore && !(strategy == Strategy::Retry && attempt <= retries) {
            context.failed_task.get_or_insert_with(|| {
                Value::map([
                    (
                        Value::string("task_id"),
                        Value::string(outcome.id.0.to_string()),
                    ),
                    (Value::string("name"), Value::string(name.as_str())),
                    (Value::string("process"), Value::string(&*self.name)),
                    (Value::string("exit"), Value::integer(outcome.exit_status)),
                    (
                        Value::string("attempt"),
                        Value::Number(nextflow_evaluator::number::Number::Long(attempt as i64)),
                    ),
                    (Value::string("status"), Value::string("FAILED")),
                ])
            });
        }
        match strategy {
            Strategy::Retry if attempt <= retries => {
                interpreter.print(&format!(
                    "NOTE: {failure} -- Execution is retried ({attempt})\n"
                ));
                self.launch(context, values, index, Some(outcome.id), attempt + 1)?;
                Ok(State::Running)
            }
            Strategy::Ignore => {
                interpreter.print(&format!("NOTE: {failure} -- Error is ignored\n"));
                self.finish_if_done(context)
            }
            Strategy::Finish => {
                context.fail(error);
                self.finish_if_done(context)
            }
            // A run that keeps going goes on as if the task was ignored, and fails at its end.
            Strategy::Terminate | Strategy::Retry if context.keep_going => {
                context.fail(error);
                self.finish_if_done(context)
            }
            Strategy::Terminate | Strategy::Retry => Err(error),
        }
    }

    /// The value one output binds for a task; `None` when an optional output is missing.
    fn output_value(
        &self,
        output: &RenderedOutput,
        outcome: &TaskOutcome,
        task: &str,
        eval_index: &mut usize,
        bound: &mut Bound,
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
                    let arity = output_arity(options)?;
                    // As Nextflow, a glob does not match the staged inputs, unless the output
                    // includes them; a name without wildcards may name one.
                    let inputs = option(options, "includeInputs").is_some_and(|v| v.truth());
                    for pattern in patterns {
                        let glob = pattern.contains(['*', '?', '[', '{']);
                        let matching = || {
                            (outcome.files.iter()).filter(|(name, _)| glob_matches(pattern, name))
                        };
                        let mut found: Vec<&(String, PathValue)> = matching()
                            .filter(|(name, _)| {
                                inputs || !glob || !bound.staged.contains(&name.as_str())
                            })
                            .collect();
                        // The simulator names outputs after their patterns, so the made-up
                        // output of a step can have the name of the made-up input it read.
                        if found.is_empty() && bound.simulated {
                            found = matching().collect();
                        }
                        if found.is_empty()
                            && !output.optional
                            && arity.is_none_or(|(minimum, _)| minimum > 0)
                        {
                            let excluded = glob
                                && !inputs
                                && (outcome.files.iter())
                                    .any(|(name, _)| glob_matches(pattern, name));
                            let note = match excluded {
                                true => {
                                    " (note: input files are not included in the default \
                                         matching set)"
                                }
                                false => "",
                            };
                            let message = format!(
                                "Missing output file(s) `{pattern}` expected by process \
                                 `{task}`{note}"
                            );
                            return Err(thrown(Exception::new("MissingFileException", message)));
                        }
                        for (name, path) in found {
                            files.push(Value::Path(Rc::new(path.clone())));
                            if !bound.files.iter().any(|(known, _)| known == name) {
                                bound.files.push((name.clone(), path.clone()));
                            }
                        }
                    }
                    if let Some((minimum, maximum)) = arity
                        && (files.len() < minimum
                            || maximum.is_some_and(|maximum| files.len() > maximum))
                    {
                        return Err(thrown(Exception::new(
                            "IllegalArityException",
                            format!(
                                "Output arity `{}` expected by process `{task}`, but found {} file(s)",
                                option(options, "arity").ok_or_else(|| thrown(Exception::new(
                                    "IllegalStateException",
                                    "Output arity disappeared during collection"
                                )))?,
                                files.len()
                            ),
                        )));
                    }
                    match (files.as_slice(), arity) {
                        ([single], None | Some((_, Some(1)))) => single.clone(),
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
        if done && self.pending.is_empty() && self.each_round.is_none() {
            for output in self.outputs.clone() {
                context.close(output);
            }
            return Ok(State::Stopped);
        }
        Ok(State::Running)
    }
}

fn output_arity(options: &Value) -> Result<Option<(usize, Option<usize>)>, Error> {
    let Some(arity) = option(options, "arity") else {
        return Ok(None);
    };
    let text = arity.to_string();
    let parsed = match text.split_once("..") {
        Some((minimum, "*")) => minimum.parse().ok().map(|minimum| (minimum, None)),
        Some((minimum, maximum)) => minimum
            .parse()
            .ok()
            .zip(maximum.parse().ok())
            .map(|(minimum, maximum)| (minimum, Some(maximum))),
        None => text.parse().ok().map(|count| (count, Some(count))),
    };
    match parsed {
        Some((minimum, maximum))
            if maximum.is_none_or(|maximum| {
                maximum >= 1 && minimum <= maximum && !(minimum == 0 && maximum == 1)
            }) =>
        {
            Ok(parsed)
        }
        _ => Err(thrown(Exception::new(
            "IllegalArgumentException",
            format!("Invalid output arity: {arity}"),
        ))),
    }
}

/// The files and folders below the folder of an `exec:` section, by their paths there, as
/// Nextflow collects the outputs of a task from its work folder.
fn made_files(folder: &Path) -> std::io::Result<Vec<(String, PathValue)>> {
    let mut made = Vec::new();
    let mut pending = vec![folder.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in std::fs::read_dir(&current)? {
            let path = entry?.path();
            let name = path
                .strip_prefix(folder)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            if path.is_dir() {
                pending.push(path.clone());
            }
            made.push((name, PathValue::parse(&path.to_string_lossy(), "/")));
        }
    }
    made.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(made)
}

/// The `task.ext` map of the config: selectors replace entries, plain settings add missing
/// ones. Closures in it are called when a script reads them.
fn ext_map(settings: &ProcessSettings) -> Result<Value, Error> {
    let ext = Value::map([]);
    let Value::Map(map) = &ext else {
        return Err(thrown(Exception::new(
            "IllegalStateException",
            "Cannot create task.ext map",
        )));
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
    Ok(ext)
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
        ("maxErrors", value) => unlimited_errors(value),
        ("errorStrategy", Value::String(text)) => text.eq_ignore_ascii_case("terminate"),
        ("shell", Value::List(list)) => {
            let items: Vec<String> = list.borrow().items.iter().map(Value::to_string).collect();
            items == ["/bin/bash", "-ue"]
        }
        _ => false,
    }
}

/// Whether a `maxErrors` value means no limit: -1, also as text like nf-core's `'-1'`, or null.
fn unlimited_errors(value: &Value) -> bool {
    match value {
        Value::Number(value) => value.to_f64() == -1.0,
        Value::String(text) => text.trim().parse::<f64>() == Ok(-1.0),
        Value::Null => true,
        _ => false,
    }
}

/// The `task` object of a task: the directives' values, with Nextflow's defaults, its attempt,
/// and after a failure its exit status.
fn task_object(
    process: &str,
    index: u32,
    attempt: u32,
    exit_status: Option<i32>,
    directives: &[TaskDirective],
    ext: &Value,
) -> Value {
    let mut entries: Vec<(Value, Value)> = vec![
        (Value::string("process"), Value::string(process)),
        (Value::string("index"), Value::integer(index as i32)),
        (Value::string("attempt"), Value::integer(attempt as i32)),
        (Value::string("cpus"), Value::integer(1)),
        (Value::string("ext"), ext.clone()),
        (
            Value::string("shell"),
            Value::list(vec![Value::string("/bin/bash"), Value::string("-ue")]),
        ),
    ];
    if let Some(status) = exit_status {
        entries.push((Value::string("exitStatus"), Value::integer(status)));
    }
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

#[cfg(test)]
mod directive_tests {
    #[test]
    fn every_lowered_directive_is_handled_or_refused() {
        let mut runtime = super::HANDLED_DIRECTIVES.to_vec();
        runtime.extend_from_slice(super::REFUSED_DIRECTIVES);
        runtime.sort_unstable();
        let mut lowered = nextflow_evaluator::lower::PROCESS_DIRECTIVES.to_vec();
        lowered.sort_unstable();
        assert_eq!(runtime, lowered);
    }
}
