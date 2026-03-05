//! Runs Nextflow workflows: the dataflow network of channels, operators and processes.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod build;
pub mod channel;
mod network;
mod operator;
mod process;

use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, Host, Interpreter};
use nextflow_evaluator::lowered::Module;
use nextflow_evaluator::value::Value;

pub use network::Network;
pub use process::{TaskId, TaskOutcome, TaskRequest};

/// Runs the entry workflow of a script: builds the network, ignites it and runs it until
/// nothing is left to do. A run that would wait forever in Nextflow is an error here.
pub fn run(module: &Rc<Module>, host: &mut dyn Host) -> Result<(), Error> {
    let mut network = Network::default();
    {
        let mut builder = build::Builder {
            network: &mut network,
            base: host,
        };
        let mut interpreter = Interpreter::new(&mut builder);
        set_globals(&mut interpreter);
        interpreter.run_entry(module)?;
    }
    let mut interpreter = Interpreter::new(host);
    set_globals(&mut interpreter);
    network.ignite(&mut interpreter)?;
    let waiting = network.running();
    if !waiting.is_empty() {
        let names: Vec<&str> = waiting.iter().map(|name| &**name).collect();
        let message = format!(
            "The run cannot finish, because these steps wait for input that never comes: {}",
            names.join(", ")
        );
        return Err(Error::Thrown {
            exception: Rc::new(Exception::new("IllegalStateException", message)),
            span: Default::default(),
        });
    }
    Ok(())
}

fn set_globals(interpreter: &mut Interpreter) {
    interpreter.set_global("channel", Value::Namespace("channel".into()));
    interpreter.set_global("log", Value::Namespace("log".into()));
}
