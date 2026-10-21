//! Runs Nextflow workflows: the dataflow network of channels, operators and processes.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod build;
pub mod channel;
pub mod driver;
mod files;
mod hash;
pub mod inputs;
mod keyed;
mod loader;
mod network;
mod notes;
mod operator;
mod options;
mod output_schema;
mod outputs;
mod pairs;
mod process;
mod publish;
mod records;
mod reports;
mod route;
mod session;
mod split;
mod store;
mod strategy;
pub mod task;
mod workflow;

use std::rc::Rc;

use nextflow_config::{Config, tree};
use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, Host, Interpreter};
use nextflow_evaluator::lowered::Module;
use nextflow_evaluator::value::{PathValue, Value};

pub use driver::driver;
pub use loader::{LoadError, Loader};
pub use network::Network;
pub use process::{TaskId, TaskOutcome, TaskRequest};

use session::Session;

/// How a run behaves.
#[derive(Clone, Debug)]
pub struct Options {
    /// The named workflow selected by `-entry`, otherwise the unnamed entry workflow.
    pub entry: Option<String>,
    /// Run the `stub:` section of processes instead of their script.
    pub stub: bool,
    /// Whether the run is simulated, so that its files are made up (`Launch.simulated`).
    pub simulated: bool,
    /// The output directory of the workflow outputs, from `-output-dir`.
    pub output_dir: Option<String>,
    /// The container engine, like `docker`, which `workflow.containerEngine` gives.
    pub container_engine: Option<String>,
    /// The merged config, with the params of the command line.
    pub config: Config,
    /// The profiles of `-profile`, as `workflow.profile` shows them.
    pub profile: String,
    /// The directory of the main script, `projectDir`.
    pub project_dir: String,
    /// The main script.
    pub script_file: String,
    /// The name of the run, `workflow.runName`.
    pub run_name: String,
    /// The config files that were read, in order.
    pub config_files: Vec<String>,
    /// The command line that started the run.
    pub command_line: String,
    /// Where the `exec:` sections of processes run: a folder of this run on this machine, with
    /// a folder for each task, its `task.workDir`. Without it, `exec:` sections are not
    /// supported.
    pub exec_folder: Option<std::path::PathBuf>,
    /// Whether the run goes on after a task failed that would end it (`Launch.keep_going`):
    /// the task counts as ignored, and the run fails at its end.
    pub keep_going: bool,
    /// The names of the params of the command line and the params file.
    pub cli_params: Vec<String>,
}

/// A run of a script: its network, which asks for tasks and takes their outcomes.
pub struct Run {
    network: Network,
    options: Options,
    /// The `workflow` object of the run.
    workflow: Value,
    on_complete_called: bool,
    on_error_called: bool,
    operator_directory: Option<PathValue>,
}

impl Run {
    /// Builds the network from the entry workflow and ignites it; the result has the first
    /// tasks to run.
    pub fn start(
        module: &Rc<Module>,
        options: &Options,
        host: &mut dyn Host,
    ) -> Result<(Run, Vec<TaskRequest>), Error> {
        let (run, requests) = Self::start_retained(module, options, host);
        Ok((run, requests?))
    }

    pub(crate) fn start_retained(
        module: &Rc<Module>,
        options: &Options,
        host: &mut dyn Host,
    ) -> (Run, Result<Vec<TaskRequest>, Error>) {
        let mut network = Network::default();
        network.keep_going(options.keep_going);
        let workflow = workflow::metadata(options, &host.launch_dir());
        let mut operator_directory = None;
        let mut session = Session {
            base: host,
            options,
            network: None,
            operator_directory: &mut operator_directory,
        };
        let started = (|| {
            {
                let mut builder = build::Builder {
                    network: &mut network,
                    base: &mut session,
                    stub: options.stub,
                    config: &options.config,
                    output_dir: options.output_dir.clone(),
                    exec_folder: options.exec_folder.clone().map(Rc::from),
                };
                let mut interpreter = Interpreter::new(&mut builder);
                set_globals(&mut interpreter, options, &workflow);
                let params = options.config.params();
                script_params(&mut interpreter, module, &params, &mut Vec::new())
                    .and_then(|()| typed_params(&mut interpreter, module, &params))
                    .and_then(|()| interpreter.start_statements(module))
                    .and_then(|()| match options.entry.as_deref() {
                        Some(name) => interpreter.run_named_entry(module, name),
                        None => interpreter.run_entry(module),
                    })
                    .map_err(|error| interpreter.locate(error))?;
            }
            let mut interpreter = Interpreter::new(&mut session);
            set_globals(&mut interpreter, options, &workflow);
            network
                .ignite(&mut interpreter)
                .map_err(|error| interpreter.locate(error))?;
            let requests = network.take_requests();
            Ok(requests)
        })();
        let run = Run {
            network,
            options: options.clone(),
            workflow,
            on_complete_called: false,
            on_error_called: false,
            operator_directory,
        };
        (run, started)
    }

    /// Takes the outcome of a task; the result has the tasks that can run now.
    pub fn complete(
        &mut self,
        outcome: TaskOutcome,
        host: &mut dyn Host,
    ) -> Result<Vec<TaskRequest>, Error> {
        let mut session = Session {
            base: host,
            options: &self.options,
            network: None,
            operator_directory: &mut self.operator_directory,
        };
        let mut interpreter = Interpreter::new(&mut session);
        set_globals(&mut interpreter, &self.options, &self.workflow);
        self.network
            .complete(outcome, &mut interpreter)
            .map_err(|error| interpreter.locate(error))?;
        Ok(self.network.take_requests())
    }

    /// The files to deliver since the last call, like those of a `publishDir`.
    pub fn take_deliveries(&mut self) -> Vec<workflow_graph::Delivery> {
        self.network.take_deliveries()
    }

    /// Consumes the native output-check verdict of the task just completed.
    pub fn take_acceptance(&mut self, task: TaskId) -> bool {
        self.network.take_acceptance(task)
    }

    /// Whether the run ends once the running tasks have ended, after a failure.
    pub fn draining(&self) -> bool {
        self.network.draining()
    }

    /// Checks that the run ended, then runs the `workflow.onComplete` handlers. No task may
    /// run and no step may wait for input: a run that would wait forever in Nextflow is an
    /// error here.
    pub fn finish(&mut self, host: &mut dyn Host) -> Result<(), Error> {
        if self.on_complete_called {
            return Ok(());
        }
        if let Some(failure) = self.network.take_failure() {
            return Err(failure);
        }
        let waiting = self.network.running();
        if waiting.is_empty() && !self.network.has_tasks() {
            workflow::finish(&self.workflow, true, self.network.succeeded);
            return self.handlers(host, false, Value::Null, false);
        }
        let names: Vec<&str> = waiting.iter().map(|name| &**name).collect();
        let message = format!(
            "The run cannot finish, because these steps wait for input that never comes: {}",
            names.join(", ")
        );
        Err(Error::Thrown {
            exception: Rc::new(Exception::new("IllegalStateException", message)),
            span: Default::default(),
        })
    }

    pub(crate) fn failed(
        &mut self,
        message: &str,
        status: i32,
        task_failure: bool,
        host: &mut dyn Host,
    ) -> Result<(), Error> {
        if self.on_error_called {
            return Ok(());
        }
        let trace = if task_failure {
            self.network.failed_task.clone().unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        let status = tree::get(&trace, &["exit"]).unwrap_or(Value::integer(status));
        workflow::finish(&self.workflow, false, self.network.succeeded);
        workflow::failure(&self.workflow, message, status);
        let error = self.handlers(host, true, trace, true).err();
        let completion_error = self.handlers(host, false, Value::Null, true).err();
        match error.or(completion_error) {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn handlers(
        &mut self,
        host: &mut dyn Host,
        on_error: bool,
        trace: Value,
        failing: bool,
    ) -> Result<(), Error> {
        let called = if on_error {
            &mut self.on_error_called
        } else {
            &mut self.on_complete_called
        };
        if std::mem::replace(called, true) {
            return Ok(());
        }
        let mut session = Session {
            base: host,
            options: &self.options,
            network: Some(&self.network),
            operator_directory: &mut self.operator_directory,
        };
        let mut interpreter = Interpreter::new(&mut session);
        set_globals(&mut interpreter, &self.options, &self.workflow);
        let mut failure = None;
        for handler in workflow::handlers(&self.workflow, on_error) {
            let arguments = if on_error {
                vec![trace.clone()]
            } else {
                Vec::new()
            };
            if let Err(error) = interpreter.call_closure(&handler, arguments) {
                let error = interpreter.locate(error);
                if !failing
                    && error.exception().is_none_or(|exception| {
                        exception.target().class == "AbortOperationException"
                    })
                {
                    return Err(error);
                }
                let name = if on_error { "onError" } else { "onComplete" };
                interpreter.print(&format!(
                    "ERROR ~ Failed to invoke `workflow.{name}` event handler: {error}\n"
                ));
                if failing {
                    failure.get_or_insert(error);
                }
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

/// Runs a script whose workflow runs no tasks.
pub fn run(module: &Rc<Module>, host: &mut dyn Host) -> Result<(), Error> {
    let (mut run, requests) = Run::start(module, &Options::default(), host)?;
    if let Some(request) = requests.first() {
        return Err(Error::Unsupported {
            message: format!("running the task `{}` without an executor", request.name),
            span: Default::default(),
        });
    }
    run.finish(host)
}

impl Default for Options {
    fn default() -> Self {
        Self {
            entry: None,
            stub: false,
            simulated: false,
            output_dir: None,
            container_engine: None,
            config: Config::default(),
            profile: "standard".into(),
            project_dir: "/".into(),
            script_file: "/main.nf".into(),
            run_name: "uwe".into(),
            config_files: Vec::new(),
            command_line: String::new(),
            exec_folder: None,
            keep_going: false,
            cli_params: Vec::new(),
        }
    }
}

/// Evaluates declared params before workflow execution, filling unset config values from scripts.
/// Typed params use evaluated defaults, or None when no default is declared.
pub fn declared_params(
    module: &Rc<Module>,
    options: &Options,
    host: &mut dyn Host,
) -> Result<(Value, Vec<Option<Value>>), Error> {
    let workflow = workflow::metadata(options, &host.launch_dir());
    let mut interpreter = Interpreter::new(host);
    set_globals(&mut interpreter, options, &workflow);
    let params = options.config.params();
    script_params(&mut interpreter, module, &params, &mut Vec::new())
        .map_err(|error| interpreter.locate(error))?;
    let mut defaults = Vec::new();
    for parameter in &module.typed_parameters {
        let default = match &parameter.default {
            Some(code) => Some(
                interpreter
                    .evaluate(code, module)
                    .map_err(|error| interpreter.locate(error))?,
            ),
            None => None,
        };
        defaults.push(default);
    }
    Ok((params, defaults))
}

/// Where the `output { }` block of a script publishes an output, below the output folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputTarget {
    pub name: Rc<str>,
    /// The folder or file that the `path` option gives; `None` when a closure decides it for
    /// each value, or when the output has no `path`.
    pub path: Option<String>,
    /// The index file of the `index { }` section, which lists the published values.
    pub index: Option<String>,
}

/// The targets of the `output { }` block of a script, with the texts of their `path` options
/// where they are texts. Nothing of the workflow runs.
pub fn output_targets(
    module: &Rc<Module>,
    options: &Options,
    host: &mut dyn Host,
) -> Vec<OutputTarget> {
    let workflow = workflow::metadata(options, &host.launch_dir());
    let mut interpreter = Interpreter::new(host);
    set_globals(&mut interpreter, options, &workflow);
    let mut text = |options: &[(Rc<str>, Rc<nextflow_evaluator::lowered::Code>)]| {
        let (_, code) = options.iter().find(|(name, _)| &**name == "path")?;
        match interpreter.evaluate(code, module).ok()? {
            value @ (Value::String(_) | Value::GString(_) | Value::Path(_)) => {
                Some(value.to_string())
            }
            _ => None,
        }
    };
    module
        .outputs
        .iter()
        .map(|output| OutputTarget {
            name: output.name.clone(),
            path: text(&output.options),
            index: text(&output.index),
        })
        .collect()
}

/// Runs the `params.x = value` statements of a script and the scripts it includes: each one
/// only sets a param that the config and the command line leave unset.
fn script_params(
    interpreter: &mut Interpreter,
    module: &Rc<Module>,
    params: &Value,
    done: &mut Vec<*const Module>,
) -> Result<(), Error> {
    if done.contains(&Rc::as_ptr(module)) {
        return Ok(());
    }
    done.push(Rc::as_ptr(module));
    for (path, code) in &module.parameters {
        let names: Vec<&str> = path.iter().map(|name| &**name).collect();
        if tree::get(params, &names).is_none() {
            let value = interpreter.evaluate(code, module)?;
            tree::assign(params, path, value).map_err(|message| Error::Thrown {
                exception: Rc::new(Exception::illegal_argument(message)),
                span: code.span,
            })?;
        }
    }
    for module in included(module) {
        script_params(interpreter, &module, params, done)?;
    }
    Ok(())
}

/// The scripts that a script includes, in the order of the includes, without those of includes
/// with `addParams(...)` or `params(...)`, which start when their include runs.
fn included(module: &Module) -> Vec<Rc<Module>> {
    let links = module.links.borrow();
    let conditional = module.conditional_links.borrow();
    let mut modules: Vec<Rc<Module>> = Vec::new();
    for (index, include) in module.includes.iter().enumerate() {
        let link = match include.conditional {
            true => conditional.get(&index),
            false => links.get(&include.alias),
        };
        if let Some(link) = link.filter(|_| !include.params)
            && !modules.iter().any(|known| Rc::ptr_eq(known, &link.module))
        {
            modules.push(link.module.clone());
        }
    }
    modules
}

/// Applies the typed `params { }` block of the main script: each param takes its value from
/// the command line or the config, converted to its type, else its default. A `Boolean`
/// without a default is `false`, a nullable type may stay `null`, other params are required.
fn typed_params(
    interpreter: &mut Interpreter,
    module: &Rc<Module>,
    params: &Value,
) -> Result<(), Error> {
    for parameter in &module.typed_parameters {
        let class = parameter.class.as_deref().unwrap_or("Object");
        let value = match tree::get(params, &[&parameter.name]) {
            Some(Value::Null) | None => match &parameter.default {
                Some(code) => interpreter.evaluate(code, module)?,
                None if class == "Boolean" => Value::Boolean(false),
                None if parameter.nullable => Value::Null,
                None => {
                    let message = format!(
                        "Parameter `{}` is required but was not specified on the command line, params file, or config",
                        parameter.name
                    );
                    return Err(Error::Thrown {
                        exception: Rc::new(Exception::illegal_argument(message)),
                        span: parameter.span,
                    });
                }
            },
            Some(Value::String(text)) => typed_value(&text, class).ok_or_else(|| {
                let message = format!(
                    "Parameter `{}` with type {class} cannot be assigned to '{text}'",
                    parameter.name
                );
                Error::Thrown {
                    exception: Rc::new(Exception::illegal_argument(message)),
                    span: parameter.span,
                }
            })?,
            Some(value) => value,
        };
        let path = [parameter.name.clone()];
        tree::assign(params, &path, value).expect("params is a map");
    }
    Ok(())
}

/// Text as a value of a declared param type, or `None` when it does not fit.
fn typed_value(text: &str, class: &str) -> Option<Value> {
    use nextflow_evaluator::number::{BigDecimal, Number};
    use nextflow_evaluator::units::{parse_duration, parse_memory};
    Some(match class {
        "Integer" => match text.trim().parse::<i32>() {
            Ok(value) => Value::integer(value),
            Err(_) => Value::Number(Number::Long(text.trim().parse().ok()?)),
        },
        "Float" | "Double" | "Number" => {
            Value::Number(Number::BigDecimal(BigDecimal::parse(text.trim())?))
        }
        "Boolean" if text.eq_ignore_ascii_case("true") => Value::Boolean(true),
        "Boolean" if text.eq_ignore_ascii_case("false") => Value::Boolean(false),
        "Boolean" => return None,
        "Path" => Value::Path(Rc::new(nextflow_evaluator::value::PathValue::parse(
            text,
            &std::env::current_dir()
                .unwrap_or_default()
                .to_string_lossy(),
        ))),
        "Duration" => Value::Duration(parse_duration(text).ok()?),
        "MemoryUnit" => Value::MemoryUnit(parse_memory(text).ok()?),
        _ => Value::string(text),
    })
}

fn set_globals(interpreter: &mut Interpreter, options: &Options, workflow: &Value) {
    interpreter.set_global("params", options.config.params());
    // The command-line arguments after the script; uwe takes none.
    interpreter.set_global("args", Value::list(Vec::new()));
    interpreter.set_global("channel", Value::Namespace("channel".into()));
    interpreter.set_global("log", Value::Namespace("log".into()));
    interpreter.set_global("workflow", workflow.clone());
    interpreter.set_global("nextflow", workflow::nextflow());
    for (name, property) in [
        ("projectDir", "projectDir"),
        ("baseDir", "projectDir"),
        ("launchDir", "launchDir"),
        ("workDir", "workDir"),
    ] {
        interpreter.set_global(name, workflow::property(workflow, property));
    }
    interpreter.set_global("secrets", Value::Namespace("secrets".into()));
}
