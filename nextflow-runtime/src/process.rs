//! Processes in the network: each round of input values becomes a task, and each task outcome
//! becomes output values (channels spec, section 7).
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use nextflow_config::ProcessSettings;
use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{
    Error, Interpreter, RenderedOutput, RenderedPart, TaskDirective, TaskFrame,
};
use nextflow_evaluator::lower::AGENT_DIRECTIVES;
use nextflow_evaluator::lowered::{Input, Module, Process};
use nextflow_evaluator::units::{parse_duration, parse_memory};
use nextflow_evaluator::value::{ChannelId, Key, ListKind, MapKind, PathValue, Value};
use nextflow_parser::ast::BodyKind;
use nextflow_plugins::observer::Part;

use crate::channel::Message;
use crate::network::{Behaviour, Context, Definition, State};
use crate::publish;
use crate::strategy::{self, Strategy};

const HANDLED_DIRECTIVES: &[&str] = &[
    "accelerator",
    "afterScript",
    "beforeScript",
    "cache",
    "conda",
    "container",
    "containerOptions",
    "cpus",
    "debug",
    "disk",
    "echo",
    "errorStrategy",
    "ext",
    "fair",
    "label",
    "maxForks",
    "maxRetries",
    "memory",
    // The executors that take pod options get them, like Kubernetes.
    "pod",
    "publishDir",
    // The executor that has queues and job labels takes them, like AWS Batch and SLURM.
    "queue",
    "resourceLabels",
    "resourceLimits",
    "secret",
    "shell",
    "stageInMode",
    // The store takes the output files whatever the mode, so every mode gives the same files.
    "stageOutMode",
    "storeDir",
    "tag",
    "time",
];

const REFUSED_DIRECTIVES: &[&str] = &["maxErrors", "module", "spack"];

const HINT_DIRECTIVES: &[&str] = &[
    "arch",
    "array",
    "clusterOptions",
    "executor",
    "hints",
    "machineType",
    "maxSubmitAwait",
    "penv",
    "scratch",
];

pub(crate) fn capabilities() -> serde_json::Value {
    serde_json::json!({
        "supported": HANDLED_DIRECTIVES,
        "unsupported": REFUSED_DIRECTIVES,
        "placement_ignored": HINT_DIRECTIVES,
        "placement_source": "uwe executor settings"
    })
}

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
    /// The value of each input declaration, for run observers.
    pub values: Vec<Value>,
    /// The parts of each output declaration, for run observers.
    pub declared: Vec<Vec<Part>>,
    pub input_environment: Vec<(Rc<str>, Value)>,
    pub stdin: Option<Value>,
    /// The glob patterns of the output files, relative to the task directory.
    pub output_patterns: Vec<String>,
    /// The names of the `env` outputs.
    pub env: Vec<Rc<str>>,
    /// The commands of the `eval` outputs, run after the script.
    pub eval: Vec<String>,
    /// Whether an output takes the standard output of the task, which is then read as text.
    pub stdout: bool,
    /// The files that an `exec:` section made in its `task.workDir`, by their names there. Such
    /// a task ran in the interpreter already; the engine only records it.
    pub made: Option<Vec<(String, PathValue)>>,
    /// The model call of an agent task, which the driver makes; the engine only records it.
    pub agent: Option<Box<crate::agent::Call>>,
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
    pub binding: Binding,
    pub outputs: Vec<ChannelId>,
    /// Whether every input is a value channel, so that one round of values starts tasks.
    pub one_round: bool,
    pub rounds: u32,
    /// The number of tasks started, which gives each task its index.
    pub tasks: u32,
    pub limits: Limits,
    /// The outputs of the tasks that wait for earlier ones, by task index.
    pub held: std::collections::BTreeMap<u32, Vec<Option<Value>>>,
    /// The index of the task whose outputs come next.
    pub next_emission: u32,
    /// Whether an input closed, so no more rounds start.
    pub poisoned: bool,
    /// The tasks in flight.
    pub pending: HashMap<TaskId, InFlight>,
    pub each_round: Option<EachRound>,
}

/// What every task of one call of a process shares: the definition, its config settings and
/// how its tasks run. Each task of the call is bound through it.
pub(crate) struct Binding {
    pub module: Rc<Module>,
    pub process: Rc<Process>,
    pub name: Rc<str>,
    /// A process whose inputs are all values, without `each` inputs, runs one task.
    pub singleton: bool,
    pub stub: bool,
    /// Where `exec:` sections run, if they may.
    pub exec_folder: Option<Rc<Path>>,
    /// The config settings of this call of the process.
    pub settings: Rc<ProcessSettings>,
    pub conda_enabled: bool,
    pub spack_enabled: bool,
}

/// The directives of a process call that its first task fixes for all its tasks.
#[derive(Default)]
pub(crate) struct Limits {
    pub max_forks: Option<usize>,
    /// The `fair` directive: the outputs of each task wait for those of the tasks before it,
    /// by task index. A task that binds nothing holds back the later ones, as in Nextflow.
    pub fair: bool,
}

/// A bound task: its request, what its completion needs, and its outputs from a `storeDir`
/// when they are there already, so that it need not run.
pub(crate) struct Asked {
    pub request: TaskRequest,
    pub flight: InFlight,
    pub stored: Option<Vec<(String, PathValue)>>,
}

/// How a finished attempt ended, by its outputs.
pub(crate) enum Collected {
    /// The task failed, with the error of its outcome when it ran well but did not finish.
    Failed(Option<Error>),
    /// An output did not find its files or values.
    Missing(Error),
    /// The value of each output in declaration order, `None` for a missing optional one, and
    /// the files that the outputs took.
    Values {
        values: Vec<Option<Value>>,
        files: Vec<(String, PathValue)>,
    },
}

/// What the error strategy of a failed attempt decides, with the failure.
pub(crate) struct Decision {
    pub strategy: Strategy,
    pub retries: u32,
    pub message: String,
    pub error: Error,
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
    pub values: Vec<Value>,
    pub index: u32,
    pub attempt: u32,
    outputs: Vec<RenderedOutput>,
    /// The frame and the `task` object of the task, for the closures of `publishDir`.
    pub frame: TaskFrame,
    pub task: Value,
    pub publish: Vec<publish::Rule>,
    store: Option<PathValue>,
}

impl InFlight {
    /// Typed outputs compute their values from their parts in the frame of the task.
    pub(crate) fn typed_values(
        &mut self,
        interpreter: &mut Interpreter,
        values: &mut [Option<Value>],
    ) -> Result<(), Error> {
        for (output, value) in self.outputs.iter().zip(values.iter_mut()) {
            if let (Some(typed), Some(Value::List(parts))) = (&output.typed, value.as_ref()) {
                let parts = parts.borrow().items.clone();
                let computed = interpreter.typed_output(&mut self.frame, typed, parts)?;
                // Nextflow gives a tuple that a typed output makes as a plain list.
                *value = Some(match &computed {
                    Value::List(list) if list.borrow().kind == ListKind::Tuple => {
                        Value::list(list.borrow().items.clone())
                    }
                    _ => computed,
                });
            }
        }
        Ok(())
    }
}

impl Behaviour for ProcessNode {
    fn ignite(&mut self, _: &mut Context) -> Result<State, Error> {
        Ok(State::Running)
    }

    fn export(&self) -> Result<crate::export::Kind, String> {
        let binding = &self.binding;
        let process = &binding.process;
        let each = (process.inputs.iter().enumerate())
            .filter(|(_, input)| matches!(input, Input::Each(_)))
            .map(|(index, _)| index)
            .collect();
        let outputs = (process.outputs.iter())
            .map(|output| crate::export::Output {
                emit: output.emit.as_deref().map(str::to_owned),
                topic: output.topic.as_deref().map(str::to_owned),
                hidden: output.hidden,
            })
            .collect();
        let call = crate::adapter::Call {
            script: binding
                .module
                .path
                .borrow()
                .as_deref()
                .unwrap_or_default()
                .to_owned(),
            process: process.name.to_string(),
            name: binding.name.to_string(),
            singleton: binding.singleton,
        };
        Ok(crate::export::Kind::Process {
            call,
            each,
            start: process.span.start,
            end: process.span.end,
            outputs,
        })
    }

    fn definition(&self) -> Option<Definition> {
        Some(Definition {
            name: self.binding.process.name.clone(),
            script: self.binding.module.path.borrow().clone(),
        })
    }

    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        if (self.limits.max_forks).is_some_and(|limit| self.pending.len() >= limit) {
            return Ok(State::Running);
        }
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
        let Some(mut flight) = self.pending.remove(&outcome.id) else {
            return Ok(State::Running);
        };
        context.accept(outcome.id, false);
        let interpreter = &mut *context.interpreter;
        let mut values = match self
            .binding
            .collect(interpreter, &mut flight, &mut outcome)?
        {
            Collected::Failed(failure) => return self.failed(context, flight, &outcome, failure),
            Collected::Missing(error) => {
                context.remember_task_files(&outcome.files);
                return self.failed(context, flight, &outcome, Some(error));
            }
            Collected::Values { values, files } => {
                context.remember_task_files(&outcome.files);
                let (frame, task) = (&flight.frame, &flight.task);
                let interpreter = &mut *context.interpreter;
                let deliveries =
                    publish::deliveries(interpreter, frame, task, &flight.publish, &files)?;
                context.deliver(deliveries);
                values
            }
        };
        flight.typed_values(context.interpreter, &mut values)?;
        context.accept(outcome.id, true);
        *context.succeeded += 1;
        if !self.limits.fair {
            self.emit(context, values);
            return self.finish_if_done(context);
        }
        self.held.insert(flight.index, values);
        while let Some(values) = self.held.remove(&self.next_emission) {
            self.emit(context, values);
            self.next_emission += 1;
        }
        self.finish_if_done(context)
    }
}

impl ProcessNode {
    /// Starts the tasks of one round of input values: one task, or with `each` inputs one
    /// task for each combination of their values, the first varying fastest, as Nextflow's
    /// `ForwardClosure` does.
    fn start_tasks(&mut self, context: &mut Context, values: Vec<Value>) -> Result<(), Error> {
        let each: Vec<usize> = (self.binding.process.inputs.iter().enumerate())
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
        let (interpreter, counter) = context.binder();
        let limits = &mut self.limits;
        let asked =
            (self.binding).prepare(interpreter, counter, limits, values, index, id, attempt)?;
        let Some(Asked {
            request,
            flight,
            stored,
        }) = asked
        else {
            return Ok(());
        };
        let id = request.id;
        self.pending.insert(id, flight);
        if let Some(files) = stored {
            self.complete(
                context,
                TaskOutcome {
                    id,
                    files,
                    ..TaskOutcome::default()
                },
            )?;
        } else {
            context.request(request);
        }
        Ok(())
    }
}

impl Binding {
    /// The directives of a task: its process's own, then those of the config, where selectors
    /// replace the process's own and plain settings fill in the rest.
    fn directives(
        &self,
        interpreter: &mut Interpreter,
        frame: &mut TaskFrame,
        base: &Value,
    ) -> Result<Vec<TaskDirective>, Error> {
        let mut directives = interpreter.directives(frame, &self.process, base)?;
        // The directives of an agent's definition win over its config, as the agent docs say.
        let agent = self.process.agent.is_some();
        let own: Vec<Rc<str>> = match agent {
            true => directives
                .iter()
                .map(|directive| directive.name.clone())
                .collect(),
            false => Vec::new(),
        };
        for (name, value) in &self.settings.selected {
            if &**name != "ext" && !own.contains(name) {
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
                // nf-nomad's setting of the process config, which the Nomad executor reads.
                ("nomadOptions", _) => false,
                // nf-jarvice's settings of the process config, which the JARVICE executor reads.
                ("apiUrl" | "vault" | "user", _) => false,
                ("clusterOptions", _) if takes_cluster(&directives) => false,
                (name, _) if agent && AGENT_DIRECTIVES.contains(&name) => false,
                (name, _) if HINT_DIRECTIVES.contains(&name) => !scheduling_hint(directive),
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
        crate::options::apply(
            &directives,
            &mut Default::default(),
            &mut Default::default(),
        )?;
        if directives.iter().any(|directive| {
            &*directive.name == "storeDir" && directive.arguments.iter().any(Value::truth)
        }) && directives.iter().any(|directive| {
            &*directive.name == "afterScript" && directive.arguments.iter().any(Value::truth)
        }) {
            return Err(Error::Unsupported {
                message:
                    "storeDir combined with afterScript requires output transfer before the hook"
                        .into(),
                span: self.process.span,
            });
        }
        if !self.conda_enabled {
            directives.retain(|directive| &*directive.name != "conda");
        }
        crate::task::secrets(&directives)?;
        Ok(directives)
    }

    /// Binds an attempt of a task, evaluates its directives and `task` object and renders it;
    /// `None` when `when:` skips it. A task without an id takes the next one of `counter`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare(
        &self,
        interpreter: &mut Interpreter,
        counter: &mut u64,
        limits: &mut Limits,
        values: Vec<Value>,
        index: u32,
        id: Option<TaskId>,
        attempt: u32,
    ) -> Result<Option<Asked>, Error> {
        // An `exec:` section runs here, in a folder of its own, which it knows as
        // `task.workDir`; so it needs its id first.
        let exec = self.runs_exec();
        let (id, work_dir) = match exec {
            true => {
                let id = id.unwrap_or_else(|| next_id(counter));
                let folder = self
                    .exec_folder
                    .as_ref()
                    .map(|folder| folder.join(format!("{}-{attempt}", id.0)));
                (Some(id), folder)
            }
            false => (id, None),
        };
        let mut frame =
            interpreter.bind_task_as(&self.module, &self.process, &self.name, &values)?;
        let ext = ext_map(&self.settings)?;
        let base = task_object(&self.name, index, attempt, None, &[], &ext);
        let directives = self.directives(interpreter, &mut frame, &base)?;
        if index == 1 && attempt == 1 {
            for directive in &directives {
                let used = (&*directive.name == "clusterOptions" && takes_cluster(&directives))
                    || (&*directive.name == "machineType" && takes_machine(&directives));
                if HINT_DIRECTIVES.contains(&directive.name.as_ref())
                    && directive.arguments.iter().any(Value::truth)
                    && !used
                {
                    interpreter.print(&format!(
                        "INFO: process directive `{}` is ignored for process `{}`; \
                         uwe settings select the backend and placement\n",
                        directive.name, self.name
                    ));
                }
            }
            limits.max_forks = directives
                .iter()
                .find(|directive| &*directive.name == "maxForks")
                .and_then(|directive| directive.arguments.last())
                .and_then(|value| value.to_string().parse().ok())
                .filter(|limit| *limit > 0)
                .or(self.process.agent.as_ref().map(|_| crate::agent::FORKS));
            limits.fair = (directives.iter())
                .find(|directive| &*directive.name == "fair")
                .and_then(|directive| directive.arguments.last())
                .is_some_and(Value::truth);
        }
        let task = task_object(&self.name, index, attempt, None, &directives, &ext);
        if let (Some(work_dir), Value::Map(map)) = (&work_dir, &task) {
            let path = PathValue::parse(&work_dir.to_string_lossy(), "/");
            (map.borrow_mut().entries)
                .insert(Key(Value::string("workDir")), Value::Path(Rc::new(path)));
        }
        if !interpreter.task_guard(&mut frame, &self.process, &task)? {
            return Ok(None);
        }
        let publish = publish::rules(interpreter, &frame, &task, &directives)?;
        let store = crate::store::directory(&directives, interpreter.host());
        let mut reused = None;
        if exec
            && let Some(directory) = &store
            && let Ok(Some(probe)) = interpreter.render_probe(&mut frame, &self.process, &task)
            && let Some(files) = crate::store::files(&probe.outputs, directory, interpreter.host())?
            && self.stored_outputs(&probe.outputs, &files, &self.name)
        {
            reused = Some((probe, files));
        }
        let (rendered, stored) = match reused {
            Some((probe, files)) => (probe, Some(files)),
            None => {
                if exec {
                    self.exec_work_dir(id.expect("exec task id"), attempt)?;
                }
                let rendered =
                    interpreter.render_body(&mut frame, &self.process, &task, self.stub)?;
                let stored = match &store {
                    Some(directory) if !exec => {
                        crate::store::files(&rendered.outputs, directory, interpreter.host())?
                    }
                    _ => None,
                };
                (rendered, stored)
            }
        };
        let id = id.unwrap_or_else(|| next_id(counter));
        let agent = match &self.process.agent {
            Some(agent) => Some(Box::new(crate::agent::call(
                &self.process,
                agent,
                &directives,
                &values,
                &frame.staged,
                &rendered.script,
            )?)),
            None => None,
        };
        let mut request = TaskRequest {
            id,
            attempt,
            process: self.name.clone(),
            name: display_name(&self.name, &directives, index, self.singleton),
            // The prompt of an agent goes to the model only, not into traces and reports.
            script: match agent {
                Some(_) => String::new(),
                None => rendered.script,
            },
            directives,
            staged: frame.staged.clone(),
            values: values.clone(),
            declared: rendered.outputs.iter().map(declared).collect(),
            input_environment: frame.input_environment.clone(),
            stdin: frame.stdin.clone(),
            output_patterns: Vec::new(),
            env: Vec::new(),
            eval: Vec::new(),
            stdout: false,
            made: match work_dir.as_ref().filter(|_| stored.is_none()) {
                Some(folder) => Some(made_files(folder).map_err(|error| {
                    thrown(Exception::new(
                        "IOException",
                        format!("cannot list the files of {}: {error}", folder.display()),
                    ))
                })?),
                None => None,
            },
            agent,
        };
        for part in rendered.outputs.iter().flat_map(|output| &output.parts) {
            match part {
                RenderedPart::Path { patterns, .. } | RenderedPart::Files { patterns, .. } => {
                    for pattern in patterns {
                        if !request.output_patterns.contains(pattern) {
                            request.output_patterns.push(pattern.clone());
                        }
                    }
                }
                RenderedPart::Env(name) => request.env.push(name.clone()),
                RenderedPart::Eval(command) => request.eval.push(command.clone()),
                RenderedPart::Stdout => request.stdout = true,
                RenderedPart::Value(_) => {}
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
            store,
        };
        let stored =
            stored.filter(|files| self.stored_outputs(&flight.outputs, files, &flight.name));
        Ok(Some(Asked {
            request,
            flight,
            stored,
        }))
    }

    /// Whether the tasks of the process run its `exec:` section, which needs a folder to run
    /// in; without one, `exec:` sections are not supported.
    fn runs_exec(&self) -> bool {
        let exec = (self.process.body.as_ref()).is_some_and(|body| body.kind == BodyKind::Exec);
        exec && !(self.stub && self.process.stub.is_some())
    }

    fn stored_outputs(
        &self,
        outputs: &[RenderedOutput],
        files: &[(String, PathValue)],
        name: &str,
    ) -> bool {
        let outcome = TaskOutcome {
            files: files.to_vec(),
            ..TaskOutcome::default()
        };
        let mut bound = Bound {
            staged: Vec::new(),
            files: Vec::new(),
            simulated: false,
        };
        let mut eval_index = 0;
        outputs.iter().all(|output| {
            Self::output_value(output, &outcome, name, &mut eval_index, &mut bound).is_ok()
        })
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

    /// Checks a finished attempt: its exit status, the answer of an agent, and the values of
    /// its outputs, after it saved them to its `storeDir`.
    pub(crate) fn collect(
        &self,
        interpreter: &mut Interpreter,
        flight: &mut InFlight,
        outcome: &mut TaskOutcome,
    ) -> Result<Collected, Error> {
        let simulated = interpreter.host().simulated();
        if outcome.failure.is_none()
            && let Some(structured) =
                (self.process.agent.as_ref()).and_then(|a| a.structured.as_ref())
        {
            let staged = &flight.frame.staged;
            match crate::agent::bind(structured, &outcome.stdout, staged, &self.name, simulated) {
                Ok(value) => flight.outputs[0].parts = vec![RenderedPart::Value(value)],
                Err(error) => outcome.failure = Some(error),
            }
        }
        if outcome.exit_status != 0 || outcome.failure.is_some() {
            let failure = if outcome.exit_status == 0 {
                outcome.failure.take()
            } else {
                None
            };
            return Ok(Collected::Failed(failure));
        }
        if !simulated && let Some(directory) = &flight.store {
            crate::store::save(&mut outcome.files, directory, interpreter.host())?;
        }
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
            simulated,
        };
        for output in outputs {
            match Self::output_value(output, outcome, task, &mut eval_index, &mut bound) {
                Ok(value) => values.push(value),
                Err(error) => return Ok(Collected::Missing(error)),
            }
        }
        let files = bound.files;
        Ok(Collected::Values { values, files })
    }

    /// The error strategy of a failed attempt, which is evaluated now, with the exit status of
    /// the task, and the message and error of the failure.
    pub(crate) fn decide(
        &self,
        interpreter: &mut Interpreter,
        flight: &mut InFlight,
        outcome: &TaskOutcome,
        failure: Option<Error>,
    ) -> Result<Decision, Error> {
        let (name, index, attempt) = (&flight.name, flight.index, flight.attempt);
        let ext = ext_map(&self.settings)?;
        let status = Some(outcome.exit_status);
        let base = task_object(&self.name, index, attempt, status, &[], &ext);
        let directives = self.directives(interpreter, &mut flight.frame, &base)?;
        let (strategy, retries) = strategy::of(&directives);
        let message = format!(
            "Process `{name}` terminated with an error exit status ({})",
            outcome.exit_status
        );
        let message = failure.as_ref().map_or(message, Error::to_string);
        let error = failure
            .unwrap_or_else(|| thrown(Exception::new("ProcessFailedException", message.clone())));
        Ok(Decision {
            strategy,
            retries,
            message,
            error,
        })
    }
}

impl ProcessNode {
    /// What happens after a task failed, by its error strategy, which is evaluated now, with
    /// the task's exit status: a retry of the task, going on without its outputs, or the end
    /// of the run, at once or once the running tasks have ended.
    fn failed(
        &mut self,
        context: &mut Context,
        mut flight: InFlight,
        outcome: &TaskOutcome,
        failure: Option<Error>,
    ) -> Result<State, Error> {
        let interpreter = &mut *context.interpreter;
        let decision = (self.binding).decide(interpreter, &mut flight, outcome, failure)?;
        let Decision {
            strategy,
            retries,
            message: failure,
            error,
        } = decision;
        let InFlight {
            name,
            values,
            index,
            attempt,
            ..
        } = flight;
        if strategy != Strategy::Ignore && !(strategy == Strategy::Retry && attempt <= retries) {
            context.failed_task.get_or_insert_with(|| {
                Value::map([
                    (
                        Value::string("task_id"),
                        Value::string(outcome.id.0.to_string()),
                    ),
                    (Value::string("name"), Value::string(name.as_str())),
                    (Value::string("process"), Value::string(&*self.binding.name)),
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
}

impl Binding {
    /// The value one output binds for a task; `None` when an optional output is missing.
    fn output_value(
        output: &RenderedOutput,
        outcome: &TaskOutcome,
        task: &str,
        eval_index: &mut usize,
        bound: &mut Bound,
    ) -> Result<Option<Value>, Error> {
        let mut parts = Vec::new();
        let mut missing = false;
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
                RenderedPart::Files {
                    patterns,
                    options,
                    single,
                } => typed_files(patterns, options, *single, outcome, task, bound)?,
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
            // An optional output is missing when a part that the task makes is, like a
            // pattern without files; a value, like an empty `meta` map, never is.
            missing |= !matches!(part, RenderedPart::Value(_)) && !value.truth();
            parts.push(value);
        }
        let missing = output.optional && missing;
        let value = if output.tuple {
            Value::list(parts)
        } else {
            parts.remove(0)
        };
        Ok(if missing { None } else { Some(value) })
    }
}

impl ProcessNode {
    /// Closes the outputs once no round can start and no task is in flight.
    /// Binds the values of the outputs of one task.
    fn emit(&self, context: &mut Context, values: Vec<Option<Value>>) {
        for (channel, value) in self.outputs.clone().into_iter().zip(values) {
            if let Some(value) = value {
                context.emit(channel, value);
            }
        }
    }

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

/// The parts of an output declaration as run observers see them.
fn declared(output: &RenderedOutput) -> Vec<Part> {
    let parts = output.parts.iter();
    (parts.map(|part| match part {
        RenderedPart::Value(value) => Part::Value(crate::inputs::to_json(value)),
        RenderedPart::Path { patterns, .. } | RenderedPart::Files { patterns, .. } => {
            Part::Files(patterns.clone())
        }
        _ => Part::Other,
    }))
    .collect()
}

/// Whether the task's executor is HTCondor or PBS Pro, whose uwe executors take all cluster
/// options of their schedulers.
fn takes_cluster(directives: &[TaskDirective]) -> bool {
    let executor = (directives.iter().rev())
        .find(|directive| &*directive.name == "executor")
        .and_then(|directive| directive.arguments.last());
    matches!(executor, Some(Value::String(name)) if matches!(&**name, "condor" | "ospool" | "pbspro"))
}

/// Whether the executor of the process is nf-jarvice's, which takes the machine type of a task.
fn takes_machine(directives: &[TaskDirective]) -> bool {
    let executor = (directives.iter().rev())
        .find(|directive| &*directive.name == "executor")
        .and_then(|directive| directive.arguments.last());
    matches!(executor, Some(Value::String(name)) if &**name == "jarvice")
}

fn scheduling_hint(directive: &TaskDirective) -> bool {
    directive
        .arguments
        .iter()
        .all(|value| match &*directive.name {
            "clusterOptions" => cluster_hint(value),
            "arch" if matches!(value, Value::Map(_)) => placement_hint(value, &["name"]),
            _ => true,
        })
}

fn placement_hint(value: &Value, names: &[&str]) -> bool {
    match value {
        Value::Map(map) => map
            .borrow()
            .entries
            .iter()
            .all(|(key, value)| !value.truth() || names.contains(&key.0.to_string().as_str())),
        Value::List(list) => list
            .borrow()
            .items
            .iter()
            .all(|value| placement_hint(value, names)),
        _ => !value.truth(),
    }
}

fn cluster_hint(value: &Value) -> bool {
    if let Value::List(list) = value {
        return list.borrow().items.iter().all(cluster_hint);
    }
    if !value.truth() {
        return true;
    }
    let Value::String(text) = value else {
        return false;
    };
    let mut words = text.split_whitespace();
    while let Some(word) = words.next() {
        if matches!(word, "--exclusive" | "--oversubscribe") {
            continue;
        }
        let (flag, value) = word
            .split_once('=')
            .map_or((word, None), |(flag, value)| (flag, Some(value)));
        if !matches!(
            flag,
            "--account"
                | "--partition"
                | "--qos"
                | "--constraint"
                | "--nodes"
                | "--nodelist"
                | "--exclude"
                | "--reservation"
                | "-q"
                | "-A"
                | "-P"
                | "-p"
        ) {
            return false;
        }
        if value
            .or_else(|| words.next())
            .is_none_or(|value| value.is_empty() || value.starts_with('-'))
        {
            return false;
        }
    }
    true
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

/// The next task number of a run.
pub(crate) fn next_id(counter: &mut u64) -> TaskId {
    *counter += 1;
    TaskId(*counter)
}

fn thrown(exception: Exception) -> Error {
    Error::Thrown {
        exception: Rc::new(exception),
        span: Default::default(),
    }
}

/// The files of `file()` or `files()` of a typed output: one file, or a set of files. An
/// optional one gives null or an empty set when nothing matches.
fn typed_files(
    patterns: &[String],
    options: &Value,
    single: bool,
    outcome: &TaskOutcome,
    task: &str,
    bound: &mut Bound,
) -> Result<Value, Error> {
    let optional = option(options, "optional").is_some_and(|value| value.truth());
    let inputs = option(options, "includeInputs").is_some_and(|value| value.truth());
    let mut files = Vec::new();
    for pattern in patterns {
        let glob = pattern.contains(['*', '?', '[', '{']);
        let matching = || (outcome.files.iter()).filter(|(name, _)| glob_matches(pattern, name));
        let mut found: Vec<&(String, PathValue)> = matching()
            .filter(|(name, _)| inputs || !glob || !bound.staged.contains(&name.as_str()))
            .collect();
        if found.is_empty() && bound.simulated {
            found = matching().collect();
        }
        if found.is_empty() && !optional {
            let message =
                format!("Missing output file(s) `{pattern}` expected by process `{task}`");
            return Err(thrown(Exception::new("MissingFileException", message)));
        }
        if single && found.len() > 1 {
            let message = format!(
                "Process output file '{pattern}' in `{task}` yielded {} files but expected only one",
                found.len()
            );
            return Err(thrown(Exception::new("IllegalArityException", message)));
        }
        for (name, path) in found {
            files.push(Value::Path(Rc::new(path.clone())));
            if !bound.files.iter().any(|(known, _)| known == name) {
                bound.files.push((name.clone(), path.clone()));
            }
        }
    }
    Ok(match single {
        true => files.into_iter().next().unwrap_or(Value::Null),
        false => Value::set(files),
    })
}

#[cfg(test)]
mod directive_tests {
    #[test]
    fn accounts_lowered_directives() {
        let mut runtime = super::HANDLED_DIRECTIVES.to_vec();
        runtime.extend_from_slice(super::REFUSED_DIRECTIVES);
        runtime.extend_from_slice(super::HINT_DIRECTIVES);
        runtime.sort_unstable();
        let mut lowered = nextflow_evaluator::lower::PROCESS_DIRECTIVES.to_vec();
        lowered.sort_unstable();
        assert_eq!(runtime, lowered);
    }
}
