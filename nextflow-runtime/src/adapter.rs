//! Runs single tasks of process calls outside the dataflow network, as wir dialect tasks do,
//! with the task binding, acceptance, retries and publication of the native driver.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::rc::Rc;

use nextflow_evaluator::interpreter::{Error, Interpreter};
use nextflow_evaluator::lowered::{Module, Process};
use nextflow_evaluator::value::{PathValue, Value};
use nextflow_plugins::observer;
use serde_json::Value as Json;
use workflow_graph::{Delivery, Driver, DriverError, Launch, Outcome, Task};

use crate::driver::{Ended, Nextflow, Settings, error};
use crate::process::{Asked, Binding, Collected, Decision, InFlight, Limits, TaskId, TaskOutcome};
use crate::session::Session;
use crate::strategy::Strategy;
use crate::{Options, bridge, publish};

/// A call of a process: the script that defines the process, its name there, and the fully
/// qualified name of the call, like `NFCORE_DEMO:DEMO:FASTQC`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Call {
    pub script: String,
    pub process: String,
    pub name: String,
    /// Whether the call runs one task: none of its inputs is a queue channel or `each`.
    pub singleton: bool,
}

/// A prepared first attempt of a task.
#[derive(Debug)]
pub enum Prepared {
    Run(Box<Task>),
    /// `when:` is false, so the task does not run.
    Skipped,
    /// The outputs are in the `storeDir` of the task already.
    Done(Completed),
}

/// How an attempt ended for its process.
#[derive(Debug)]
pub enum Completed {
    /// The value of each output in declaration order, `None` for a missing optional one, the
    /// files that `publishDir` publishes, and the files of the task by location with their
    /// names in its work folder, which workflow outputs publish by.
    Accepted {
        outputs: Vec<Option<Json>>,
        deliveries: Vec<Delivery>,
        names: Vec<(String, String)>,
    },
    /// The next attempt, which keeps the task id.
    Retry(Box<Task>),
    /// The error strategy ignores the failure, or `when:` skips the next attempt.
    Ignored(String),
    Failed(String),
}

/// The loaded script and config of one source context, whose process calls run task by task.
pub struct Adapter {
    driver: Nextflow,
    module: Rc<Module>,
    options: Options,
    workflow: Value,
    operator_directory: Option<PathValue>,
    calls: HashMap<Call, Rc<Binding>>,
    /// The `maxForks` and `fair` of each call, which its first task fixes, by call name.
    limits: HashMap<Rc<str>, Limits>,
    flights: HashMap<TaskId, (Rc<Binding>, InFlight)>,
}

impl Adapter {
    /// Loads the script and its config for a launch, with `params` over those of the launch,
    /// and sets the params of the scripts as a run does, without running the workflow.
    pub fn new(launch: &Launch, settings: Settings, params: Json) -> Result<Adapter, DriverError> {
        let mut driver = Nextflow::new(launch, settings)?;
        driver.use_inputs(params)?;
        let loaded = driver.setup()?;
        let options = driver.options(&loaded, loaded.config.clone());
        let workflow = crate::workflow::metadata(&options, &driver.host().launch_dir());
        let mut adapter = Adapter {
            driver,
            module: loaded.module,
            options,
            workflow,
            operator_directory: None,
            calls: HashMap::new(),
            limits: HashMap::new(),
            flights: HashMap::new(),
        };
        let (module, params) = (adapter.module.clone(), adapter.options.config.params());
        adapter.interpret(|interpreter| {
            crate::script_params(interpreter, &module, &params, &mut Vec::new())?;
            crate::typed_params(interpreter, &module, &params)
        })?;
        Ok(adapter)
    }

    /// The first attempt of the task of a call with these input values, by declaration, and
    /// this index, which counts the tasks of the call from 1.
    pub fn prepare(
        &mut self,
        call: &Call,
        inputs: &[Json],
        index: u32,
        id: workflow_graph::TaskId,
    ) -> Result<Prepared, DriverError> {
        let binding = self.binding(call)?;
        let values = inputs.iter().map(bridge::from_json).collect();
        self.attempt(binding, values, index, TaskId(id.0), 1)
    }

    /// The `maxForks` and `fair` directives of a call, once its first task fixed them.
    pub fn limits(&self, call: &Call) -> (Option<usize>, bool) {
        let limits = self
            .limits
            .get(call.name.as_str())
            .copied()
            .unwrap_or_default();
        (limits.max_forks, limits.fair)
    }

    /// The script that defines the process of this name at these bytes, which a compiled
    /// document names by its source id only.
    pub fn script_at(&self, name: &str, start: u64, end: u64) -> Option<String> {
        let mut pending = vec![self.module.clone()];
        let mut seen: Vec<Rc<Module>> = Vec::new();
        while let Some(module) = pending.pop() {
            if seen.iter().any(|known| Rc::ptr_eq(known, &module)) {
                continue;
            }
            seen.push(module.clone());
            let span = |process: &Rc<Process>| {
                (u64::from(process.span.start), u64::from(process.span.end)) == (start, end)
            };
            if module.processes.iter().any(|p| &*p.name == name && span(p)) {
                return module.path.borrow().as_deref().map(str::to_owned);
            }
            let links = module.links.borrow();
            let conditional = module.conditional_links.borrow();
            let linked = links.values().chain(conditional.values());
            pending.extend(linked.map(|link| link.module.clone()));
        }
        None
    }

    /// Takes the outcome of an attempt that [`Adapter::prepare`] or a retry asked for.
    pub fn complete(&mut self, outcome: Outcome) -> Result<Completed, DriverError> {
        let id = TaskId(outcome.id.0);
        let Ended { outcome, heard, .. } = self.driver.ended(outcome)?;
        let (binding, flight) = self
            .flights
            .remove(&id)
            .ok_or_else(|| DriverError::new(format!("the task {} was not asked for", id.0)))?;
        let completed = self.finish(binding, flight, outcome)?;
        let accepted = matches!(completed, Completed::Accepted { .. });
        self.driver.reports_ended(heard, accepted);
        Ok(completed)
    }

    /// Starts the observers and reports of the run, which the config turns on, with the
    /// process graph of the pipeline that the compiler recorded.
    pub fn start(&mut self, processes: Vec<observer::Process>) {
        self.driver.reports_started(&self.options, processes);
    }

    /// Tells the reports that an attempt started.
    pub fn started(&mut self, task: workflow_graph::TaskId, attempt: u32) {
        Driver::started(&mut self.driver, task, attempt);
    }

    /// Tells the reports that a file was delivered, like one that `publishDir` publishes.
    pub fn delivered(&mut self, delivery: &Delivery) -> Result<(), DriverError> {
        self.driver.delivered(delivery)
    }

    /// Ends the run, with the error that failed it: writes the reports and gives the files to
    /// deliver, as the native driver does at the end of a run.
    pub fn end(&mut self, error: Option<&str>) -> Result<Vec<Delivery>, DriverError> {
        self.driver.reports_finished(error)?;
        Ok(self.driver.deliveries())
    }

    fn binding(&mut self, call: &Call) -> Result<Rc<Binding>, DriverError> {
        if let Some(binding) = self.calls.get(call) {
            return Ok(binding.clone());
        }
        let Some((module, process)) = find(&self.module, &call.script, &call.process) else {
            let message = format!("no process {} in {}", call.process, call.script);
            return Err(DriverError::new(message));
        };
        if process.agent.is_some() {
            return Err(DriverError {
                unsupported: true,
                message: format!("the agent {} as a single task", call.name),
            });
        }
        let options = &self.options;
        let exec = options.exec_folder.clone().map(Rc::from);
        let definition = (&module, &process, call.name.as_str());
        let binding = crate::build::binding(
            &options.config,
            definition,
            call.singleton,
            options.stub,
            exec,
        );
        let binding = Rc::new(binding.map_err(error)?);
        self.calls.insert(call.clone(), binding.clone());
        Ok(binding)
    }

    fn attempt(
        &mut self,
        binding: Rc<Binding>,
        values: Vec<Value>,
        index: u32,
        id: TaskId,
        attempt: u32,
    ) -> Result<Prepared, DriverError> {
        let mut limits = self.limits.get(&binding.name).copied().unwrap_or_default();
        let asked = self.interpret(|interpreter| {
            let given = Some(id);
            binding.prepare(
                interpreter,
                &mut 0,
                &mut limits,
                values,
                index,
                given,
                attempt,
            )
        })?;
        self.limits.insert(binding.name.clone(), limits);
        let Some(Asked {
            request,
            flight,
            stored,
        }) = asked
        else {
            return Ok(Prepared::Skipped);
        };
        if let Some(files) = stored {
            let outcome = TaskOutcome {
                id,
                files,
                ..TaskOutcome::default()
            };
            return self.finish(binding, flight, outcome).map(Prepared::Done);
        }
        let task = self.driver.tasks(vec![request])?.pop();
        let task = task.ok_or_else(|| DriverError::new("the task was not asked for"))?;
        self.flights.insert(id, (binding, flight));
        Ok(Prepared::Run(Box::new(task)))
    }

    /// Accepts an attempt by its outputs, or decides by the error strategy what comes next.
    fn finish(
        &mut self,
        binding: Rc<Binding>,
        mut flight: InFlight,
        mut outcome: TaskOutcome,
    ) -> Result<Completed, DriverError> {
        enum Next {
            Done(Completed),
            Retry,
        }
        let next = self.interpret(|interpreter| {
            let failure = match binding.collect(interpreter, &mut flight, &mut outcome)? {
                Collected::Values { mut values, files } => {
                    let (frame, task) = (&flight.frame, &flight.task);
                    let deliveries =
                        publish::deliveries(interpreter, frame, task, &flight.publish, &files)?;
                    flight.typed_values(interpreter, &mut values)?;
                    let outputs = values
                        .iter()
                        .map(|value| value.as_ref().map(bridge::to_json));
                    let outputs = outputs.map(Option::transpose).collect::<Result<_, _>>();
                    let outputs = outputs.map_err(|message| Error::Unsupported {
                        message: format!("an output value: {message}"),
                        span: Default::default(),
                    })?;
                    let names = (outcome.files.iter())
                        .map(|(name, path)| (path.path.clone(), name.clone()))
                        .collect();
                    let accepted = Completed::Accepted {
                        outputs,
                        deliveries,
                        names,
                    };
                    return Ok(Next::Done(accepted));
                }
                Collected::Failed(failure) => failure,
                Collected::Missing(error) => Some(error),
            };
            let decision = binding.decide(interpreter, &mut flight, &outcome, failure)?;
            let Decision {
                strategy,
                retries,
                message,
                ..
            } = decision;
            let attempt = flight.attempt;
            Ok(match strategy {
                Strategy::Retry if attempt <= retries => {
                    let note = format!("NOTE: {message} -- Execution is retried ({attempt})\n");
                    interpreter.print(&note);
                    Next::Retry
                }
                Strategy::Ignore => {
                    interpreter.print(&format!("NOTE: {message} -- Error is ignored\n"));
                    Next::Done(Completed::Ignored(message))
                }
                _ => Next::Done(Completed::Failed(message)),
            })
        })?;
        if let Next::Done(completed) = next {
            return Ok(completed);
        }
        let (values, index, attempt) = (flight.values, flight.index, flight.attempt + 1);
        Ok(
            match self.attempt(binding, values, index, outcome.id, attempt)? {
                Prepared::Run(task) => Completed::Retry(task),
                Prepared::Skipped => Completed::Ignored("`when:` skips the next attempt".into()),
                Prepared::Done(completed) => completed,
            },
        )
    }

    /// Runs code in an interpreter with the globals of the run, as the network runs closures.
    /// Runs code in an interpreter of the run: its params, `workflow` and other globals, and
    /// its host, which calls plugins.
    pub fn evaluate<T>(
        &mut self,
        code: impl FnOnce(&mut Interpreter) -> Result<T, Error>,
    ) -> Result<T, DriverError> {
        self.interpret(code)
    }

    /// The main script of the run.
    pub fn script(&self) -> Option<String> {
        self.module.path.borrow().as_deref().map(str::to_owned)
    }

    fn interpret<T>(
        &mut self,
        code: impl FnOnce(&mut Interpreter) -> Result<T, Error>,
    ) -> Result<T, DriverError> {
        let mut session = Session {
            base: self.driver.host(),
            options: &self.options,
            network: None,
            operator_directory: &mut self.operator_directory,
        };
        let mut interpreter = Interpreter::new(&mut session);
        crate::set_globals(&mut interpreter, &self.options, &self.workflow);
        code(&mut interpreter).map_err(|failure| error(interpreter.locate(failure)))
    }
}

/// The module that a script path names, among the main script and the scripts it includes,
/// with its process of that name.
/// A path without `.` and `..` parts, as includes like `../modules/x` leave them in.
fn normal(path: &str) -> std::path::PathBuf {
    let mut normal = std::path::PathBuf::new();
    for part in std::path::Path::new(path).components() {
        match part {
            std::path::Component::ParentDir => {
                normal.pop();
            }
            std::path::Component::CurDir => {}
            other => normal.push(other),
        }
    }
    normal
}

fn find(main: &Rc<Module>, script: &str, name: &str) -> Option<(Rc<Module>, Rc<Process>)> {
    let script = normal(script);
    let mut pending = vec![main.clone()];
    let mut seen: Vec<Rc<Module>> = Vec::new();
    while let Some(module) = pending.pop() {
        if seen.iter().any(|known| Rc::ptr_eq(known, &module)) {
            continue;
        }
        seen.push(module.clone());
        if module.path.borrow().as_deref().map(normal) == Some(script.clone())
            && let Some(process) = module.processes.iter().find(|p| &*p.name == name)
        {
            return Some((module.clone(), process.clone()));
        }
        let links = module.links.borrow();
        let conditional = module.conditional_links.borrow();
        let linked = links.values().chain(conditional.values());
        pending.extend(linked.map(|link| link.module.clone()));
    }
    None
}
