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
mod workflow;

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
        };
        {
            let mut builder = build::Builder {
                network: &mut network,
                base: &mut session,
                stub: options.stub,
                config: &options.config,
            };
            let mut interpreter = Interpreter::new(&mut builder);
            set_globals(&mut interpreter, options, &workflow);
            let params = options.config.params();
            script_params(&mut interpreter, module, &params, &mut Vec::new())
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

    /// Checks that the run ended, then runs the `workflow.onComplete` handlers. No task may
    /// run and no step may wait for input: a run that would wait forever in Nextflow is an
    /// error here.
    pub fn finish(&mut self, host: &mut dyn Host) -> Result<(), Error> {
        let waiting = self.network.running();
        if waiting.is_empty() && !self.network.has_tasks() {
            workflow::finish(&self.workflow, true, self.succeeded);
            let mut session = Session {
                base: host,
                options: &self.options,
            };
            let mut interpreter = Interpreter::new(&mut session);
            set_globals(&mut interpreter, &self.options, &self.workflow);
            for handler in workflow::handlers(&self.workflow, false) {
                interpreter
                    .call_closure(&handler, Vec::new())
                    .map_err(|error| interpreter.locate(error))?;
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

fn set_globals(interpreter: &mut Interpreter, options: &Options, workflow: &Value) {
    interpreter.set_global("params", options.config.params());
    // The command-line arguments after the script; iunctus takes none.
    interpreter.set_global("args", Value::list(Vec::new()));
    interpreter.set_global("channel", Value::Namespace("channel".into()));
    interpreter.set_global("log", Value::Namespace("log".into()));
    interpreter.set_global("workflow", workflow.clone());
    interpreter.set_global("nextflow", workflow::nextflow());
}
