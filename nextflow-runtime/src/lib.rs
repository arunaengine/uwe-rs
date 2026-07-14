//! Runs Nextflow workflows: the dataflow network of channels, operators and processes.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod build;
pub mod channel;
pub mod driver;
mod files;
pub mod inputs;
mod keyed;
mod loader;
mod network;
mod notes;
mod operator;
mod outputs;
mod pairs;
mod process;
mod publish;
mod reports;
mod route;
mod session;
mod split;
mod strategy;
pub mod task;
mod workflow;

use std::rc::Rc;

use nextflow_config::{Config, tree};
use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, Host, Interpreter};
use nextflow_evaluator::lowered::Module;
use nextflow_evaluator::value::Value;

pub use driver::driver;
pub use loader::{LoadError, Loader};
pub use network::Network;
pub use process::{TaskId, TaskOutcome, TaskRequest};

use session::Session;

/// How a run behaves.
#[derive(Clone, Debug)]
pub struct Options {
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
}

/// A run of a script: its network, which asks for tasks and takes their outcomes.
pub struct Run {
    network: Network,
    options: Options,
    /// The `workflow` object of the run.
    workflow: Value,
    /// The number of tasks that succeeded.
    succeeded: i64,
}

impl Run {
    /// Builds the network from the entry workflow and ignites it; the result has the first
    /// tasks to run.
    pub fn start(
        module: &Rc<Module>,
        options: &Options,
        host: &mut dyn Host,
    ) -> Result<(Run, Vec<TaskRequest>), Error> {
        let mut network = Network::default();
        let workflow = workflow::metadata(options, &host.launch_dir());
        let mut session = Session {
            base: host,
            options,
            network: None,
        };
        {
            let mut builder = build::Builder {
                network: &mut network,
                base: &mut session,
                stub: options.stub,
                config: &options.config,
                output_dir: options.output_dir.clone(),
            };
            let mut interpreter = Interpreter::new(&mut builder);
            set_globals(&mut interpreter, options, &workflow);
            let params = options.config.params();
            script_params(&mut interpreter, module, &params, &mut Vec::new())
                .and_then(|()| typed_params(&mut interpreter, module, &params))
                .and_then(|()| interpreter.start_statements(module))
                .and_then(|()| interpreter.run_entry(module))
                .map_err(|error| interpreter.locate(error))?;
        }
        let mut interpreter = Interpreter::new(&mut session);
        set_globals(&mut interpreter, options, &workflow);
        network
            .ignite(&mut interpreter)
            .map_err(|error| interpreter.locate(error))?;
        let requests = network.take_requests();
        let run = Run {
            network,
            options: options.clone(),
            workflow,
            succeeded: 0,
        };
        Ok((run, requests))
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
        };
        if outcome.exit_status == 0 {
            self.succeeded += 1;
        }
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

    /// Checks that the run ended, then runs the `workflow.onComplete` handlers. No task may
    /// run and no step may wait for input: a run that would wait forever in Nextflow is an
    /// error here.
    pub fn finish(&mut self, host: &mut dyn Host) -> Result<(), Error> {
        if let Some(failure) = self.network.take_failure() {
            return Err(failure);
        }
        let waiting = self.network.running();
        if waiting.is_empty() && !self.network.has_tasks() {
            workflow::finish(&self.workflow, true, self.succeeded);
            let mut session = Session {
                base: host,
                options: &self.options,
                network: Some(&self.network),
            };
            let mut interpreter = Interpreter::new(&mut session);
            set_globals(&mut interpreter, &self.options, &self.workflow);
            for handler in workflow::handlers(&self.workflow, false) {
                // As Nextflow, an error of a handler is logged and the run still succeeds,
                // unless the handler calls `error`.
                if let Err(error) = interpreter.call_closure(&handler, Vec::new()) {
                    let error = interpreter.locate(error);
                    match error.exception() {
                        Some(exception) if exception.class != "AbortOperationException" => {
                            interpreter.print(&format!(
                                "ERROR ~ Failed to invoke `workflow.onComplete` event handler: \
                                 {error}\n"
                            ));
                        }
                        _ => return Err(error),
                    }
                }
            }
            return Ok(());
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
            stub: false,
            simulated: false,
            output_dir: None,
            container_engine: None,
            config: Config::default(),
            profile: "standard".into(),
            project_dir: "/".into(),
            script_file: "/main.nf".into(),
            run_name: "iunctus".into(),
            config_files: Vec::new(),
            command_line: String::new(),
        }
    }
}

/// What a script declares about its params, before any of its workflow runs: the params of the
/// config with those of the `params.x = value` statements of the script and its includes where
/// the config leaves them unset, and the evaluated default of each param of the typed `params`
/// block, `None` for one without a default.
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
    // The command-line arguments after the script; iunctus takes none.
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
    // Secrets are not supported; reading one gives `null`, as a missing secret does.
    interpreter.set_global("secrets", Value::map([]));
}
