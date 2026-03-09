//! Runs Nextflow workflows: the dataflow network of channels, operators and processes.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod build;
pub mod channel;
mod loader;
mod network;
mod operator;
mod process;
mod session;
pub mod task;

use std::rc::Rc;

use nextflow_config::{Config, tree};
use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, Host, Interpreter};
use nextflow_evaluator::lowered::Module;
use nextflow_evaluator::value::Value;

pub use loader::{LoadError, Loader};
pub use network::Network;
pub use process::{TaskId, TaskOutcome, TaskRequest};

use session::Session;

/// How a run behaves.
#[derive(Clone, Debug)]
pub struct Options {
    /// Run the `stub:` section of processes instead of their script.
    pub stub: bool,
    /// The container engine, like `docker`, which `workflow.containerEngine` gives.
    pub container_engine: Option<String>,
    /// The merged config, with the params of the command line.
    pub config: Config,
    /// The profiles of `-profile`, as `workflow.profile` shows them.
    pub profile: String,
    /// The directory of the main script, `projectDir`.
    pub project_dir: String,
}

/// A run of a script: its network, which asks for tasks and takes their outcomes.
pub struct Run {
    network: Network,
    options: Options,
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
        let mut session = Session {
            base: host,
            options,
        };
        {
            let mut builder = build::Builder {
                network: &mut network,
                base: &mut session,
                stub: options.stub,
                config: &options.config,
            };
            let mut interpreter = Interpreter::new(&mut builder);
            set_globals(&mut interpreter, options);
            let params = options.config.params();
            script_params(&mut interpreter, module, &params, &mut Vec::new())?;
            interpreter.run_entry(module)?;
        }
        let mut interpreter = Interpreter::new(&mut session);
        set_globals(&mut interpreter, options);
        network.ignite(&mut interpreter)?;
        let requests = network.take_requests();
        let options = options.clone();
        Ok((Run { network, options }, requests))
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
        };
        let mut interpreter = Interpreter::new(&mut session);
        set_globals(&mut interpreter, &self.options);
        self.network.complete(outcome, &mut interpreter)?;
        Ok(self.network.take_requests())
    }

    /// Checks that the run ended: no task runs and no step waits for input. A run that would
    /// wait forever in Nextflow is an error here.
    pub fn finish(&self) -> Result<(), Error> {
        let waiting = self.network.running();
        if waiting.is_empty() && !self.network.has_tasks() {
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
    let (run, requests) = Run::start(module, &Options::default(), host)?;
    if let Some(request) = requests.first() {
        return Err(Error::Unsupported {
            message: format!("running the task `{}` without an executor", request.name),
            span: Default::default(),
        });
    }
    run.finish()
}

impl Default for Options {
    fn default() -> Self {
        Self {
            stub: false,
            container_engine: None,
            config: Config::default(),
            profile: "standard".into(),
            project_dir: "/".into(),
        }
    }
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
    let included: Vec<Rc<Module>> = module
        .links
        .borrow()
        .values()
        .map(|link| link.module.clone())
        .collect();
    for module in included {
        script_params(interpreter, &module, params, done)?;
    }
    Ok(())
}

fn set_globals(interpreter: &mut Interpreter, options: &Options) {
    interpreter.set_global("params", options.config.params());
    interpreter.set_global("channel", Value::Namespace("channel".into()));
    interpreter.set_global("log", Value::Namespace("log".into()));
    let engine = options
        .container_engine
        .as_deref()
        .map_or(Value::Null, Value::string);
    let workflow = Value::map([
        (Value::string("containerEngine"), engine),
        (Value::string("stubRun"), Value::Boolean(options.stub)),
        (
            Value::string("profile"),
            Value::string(options.profile.as_str()),
        ),
    ]);
    interpreter.set_global("workflow", workflow);
}
