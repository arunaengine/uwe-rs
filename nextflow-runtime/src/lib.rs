//! Runs Nextflow workflows: the dataflow network of channels, operators and processes.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod build;
pub mod channel;
mod loader;
mod network;
mod operator;
mod process;
pub mod task;

use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, Host, Interpreter};
use nextflow_evaluator::lowered::Module;
use nextflow_evaluator::value::Value;

pub use loader::{LoadError, Loader};
pub use network::Network;
pub use process::{TaskId, TaskOutcome, TaskRequest};

/// How a run behaves.
#[derive(Clone, Debug)]
pub struct Options {
    /// Run the `stub:` section of processes instead of their script.
    pub stub: bool,
    /// The container engine, like `docker`, which `workflow.containerEngine` gives.
    pub container_engine: Option<String>,
    /// The params of the config and the command line, a map.
    pub params: Value,
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
        {
            let mut builder = build::Builder {
                network: &mut network,
                base: host,
                stub: options.stub,
            };
            let mut interpreter = Interpreter::new(&mut builder);
            set_globals(&mut interpreter, options);
            interpreter.run_entry(module)?;
        }
        let mut interpreter = Interpreter::new(host);
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
        let mut interpreter = Interpreter::new(host);
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
            params: Value::map([]),
        }
    }
}

fn set_globals(interpreter: &mut Interpreter, options: &Options) {
    interpreter.set_global("params", options.params.clone());
    interpreter.set_global("channel", Value::Namespace("channel".into()));
    interpreter.set_global("log", Value::Namespace("log".into()));
    let engine = options
        .container_engine
        .as_deref()
        .map_or(Value::Null, Value::string);
    let workflow = Value::map([
        (Value::string("containerEngine"), engine),
        (Value::string("stubRun"), Value::Boolean(options.stub)),
    ]);
    interpreter.set_global("workflow", workflow);
}
