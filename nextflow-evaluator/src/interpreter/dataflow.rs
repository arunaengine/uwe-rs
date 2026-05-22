//! The build phase of a run: calls of processes and workflows, operators on channels, pipes,
//! and `.out`. The host wires the network; workflow bodies run here.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use nextflow_parser::Span;

use super::statement::Completion;
use super::{Error, Frame, Interpreter, thrown, unsupported};
use crate::exception::Exception;
use crate::lowered::{Module, Place, Workflow};
use crate::value::{ChannelOut, DefinitionRef, Value};

impl Interpreter<'_> {
    /// Runs the entry workflow of a script, which builds the dataflow network.
    pub fn run_entry(&mut self, module: &Rc<Module>) -> Result<(), Error> {
        let entry = module
            .workflows
            .iter()
            .find(|workflow| workflow.name.is_none())
            .cloned();
        match entry {
            Some(workflow) => self
                .call_workflow(module, &workflow, Vec::new(), workflow.span)
                .map(drop),
            None => Err(unsupported(
                "scripts without an entry workflow",
                Span::default(),
            )),
        }
    }

    /// Calls a process, workflow or included function by its name in a script.
    pub(super) fn call_definition(
        &mut self,
        module: &Rc<Module>,
        name: &str,
        arguments: Vec<Value>,
        span: Span,
    ) -> Result<Value, Error> {
        let (target, target_name) = resolve(module, name);
        if let Some(plugin) = &target.plugin {
            return self
                .host
                .plugin_function(plugin, &target_name, arguments)
                .map_err(|error| error.or_at(span));
        }
        let process = target
            .processes
            .iter()
            .find(|process| *process.name == *target_name)
            .cloned();
        let workflow = target
            .workflows
            .iter()
            .find(|workflow| workflow.name.as_deref() == Some(&*target_name))
            .cloned();
        // A definition is known by the name it is called with, which may be an include alias.
        let mut path = self.workflow_path.clone();
        path.push(name.into());
        let qualified = path.join(":");
        let outputs = match (process, workflow) {
            (Some(process), _) => {
                // A closure argument gives its result, as Nextflow's process inputs take it.
                let mut values = Vec::new();
                for argument in arguments {
                    values.push(match argument {
                        Value::Closure(closure) => {
                            self.call_closure_value(&closure, Vec::new(), span)?
                        }
                        other => other,
                    });
                }
                self.host
                    .call_process(&target, &process, &qualified, values)
                    .map_err(|error| error.or_at(span))?
            }
            (None, Some(workflow)) => {
                self.workflow_path.push(name.into());
                let result = self.call_workflow(&target, &workflow, arguments, span);
                self.workflow_path.pop();
                result?
            }
            (None, None) => {
                return self.call_script_function(&target, &target_name, arguments, span);
            }
        };
        let key = (Rc::as_ptr(module) as usize, Rc::from(name));
        self.outputs.insert(key, outputs.clone());
        Ok(outputs)
    }

    /// Runs a workflow body with its takes bound; the result is its emits. A plain value
    /// that a workflow emits becomes a value channel.
    fn call_workflow(
        &mut self,
        module: &Rc<Module>,
        workflow: &Rc<Workflow>,
        arguments: Vec<Value>,
        span: Span,
    ) -> Result<Value, Error> {
        let name = workflow.name.as_deref().unwrap_or("<entry>");
        if arguments.len() != workflow.takes.len() {
            let message = format!(
                "Workflow `{name}` declares {} input channels but {} were given",
                workflow.takes.len(),
                arguments.len()
            );
            return Err(thrown(Exception::illegal_argument(message), span));
        }
        let mut frame = Frame {
            module: module.clone(),
            slots: (0..workflow.slot_count)
                .map(|_| Rc::new(RefCell::new(Value::Null)))
                .collect(),
            captured: Vec::new(),
            unset: RefCell::default(),
        };
        for ((_, slot), argument) in workflow.takes.iter().zip(arguments) {
            frame.set(Place::Local(*slot), argument);
        }
        // A `return` ends the body early.
        let returned = matches!(
            self.block(&mut frame, &workflow.main)?,
            Completion::Return(_)
        );
        for (name, expression) in &workflow.publishers {
            let value = match self.expression(&mut frame, expression)? {
                Value::ChannelOut(out) if out.values.len() == 1 => out.values[0].clone(),
                Value::ChannelOut(_) => {
                    let message = format!("Cannot publish a multi-channel output to `{name}`");
                    return Err(thrown(
                        Exception::illegal_argument(message),
                        expression.span,
                    ));
                }
                value @ Value::Channel(_) => value,
                plain => self.namespace_call("channel", "value", vec![plain], expression.span)?,
            };
            self.host
                .publish(name, value)
                .map_err(|error| error.or_at(expression.span))?;
        }
        let mut values = Vec::new();
        let mut names = Vec::new();
        for (index, (emit, expression)) in workflow.emits.iter().enumerate() {
            // After a `return`, Nextflow finds only the emits that name a variable the body
            // has set.
            let bare = workflow.bare_emits.get(index).copied().unwrap_or(false);
            let value = if returned && !bare {
                Value::Null
            } else {
                self.expression(&mut frame, expression)?
            };
            if returned && matches!(value, Value::Null) {
                let message = format!("Missing workflow output parameter: {emit}");
                return Err(thrown(
                    Exception::new("MissingValueException", message),
                    span,
                ));
            }
            // The outputs of a call give their one channel, as Nextflow's `collectOutputs`.
            let value = match value {
                Value::ChannelOut(out) if out.values.len() == 1 => out.values[0].clone(),
                Value::ChannelOut(out) => {
                    let (class, message) = if out.values.is_empty() {
                        (
                            "MissingValueException",
                            format!("Cannot emit empty output: {emit}"),
                        )
                    } else {
                        (
                            "IllegalArgumentException",
                            format!("Cannot emit a multi-channel output: {emit}"),
                        )
                    };
                    return Err(thrown(Exception::new(class, message), expression.span));
                }
                value @ Value::Channel(_) => value,
                plain => self.namespace_call("channel", "value", vec![plain], expression.span)?,
            };
            values.push(value);
            names.push(Some(emit.clone()));
        }
        Ok(Value::ChannelOut(Rc::new(ChannelOut { values, names })))
    }

    /// `FOO.out`: the outputs of the last call of `FOO` in this script.
    pub(super) fn definition_property(
        &mut self,
        definition: &DefinitionRef,
        name: &str,
        span: Span,
    ) -> Result<Value, Error> {
        if name != "out" {
            return Err(unsupported(
                format!("the property `{name}` of a process"),
                span,
            ));
        }
        let key = (
            Rc::as_ptr(&definition.module) as usize,
            definition.name.clone(),
        );
        match self.outputs.get(&key) {
            Some(outputs) => Ok(outputs.clone()),
            None => {
                let message = format!(
                    "Access to '{0}.out' is undefined since the process '{0}' has not been invoked \
                     before accessing the output attribute",
                    definition.name
                );
                Err(thrown(
                    Exception::new("IllegalStateException", message),
                    span,
                ))
            }
        }
    }

    /// `left | right`: channels piped into a process, workflow or operator. The outputs of a
    /// call are spread into separate arguments.
    pub(super) fn pipe(&mut self, left: Value, right: &Value, span: Span) -> Result<Value, Error> {
        match right {
            Value::Definition(definition) => {
                let arguments = match &left {
                    Value::ChannelOut(out) => out.values.clone(),
                    _ => vec![left],
                };
                self.call_definition(&definition.module, &definition.name, arguments, span)
            }
            Value::Operator(operator) => {
                self.operator(&left, &operator.0, operator.1.clone(), span)
            }
            other => Err(unsupported(
                format!("piping into {}", other.class_name()),
                span,
            )),
        }
    }

    /// An operator on a channel or on the outputs of a call.
    pub(super) fn operator(
        &mut self,
        source: &Value,
        name: &str,
        arguments: Vec<Value>,
        span: Span,
    ) -> Result<Value, Error> {
        // GPars' `length()`: how many values a channel holds now. Channels are empty while
        // the workflow is built, and closures at run time cannot see them, so it is 0, which
        // Nextflow also gives when the value is not there yet.
        if let (Value::Channel(_), "length", []) = (source, name, arguments.as_slice()) {
            return Ok(Value::integer(0));
        }
        match self
            .host
            .operator(source, name, arguments)
            .map_err(|error| error.or_at(span))?
        {
            Some(value) => Ok(value),
            None => Err(unsupported(format!("the operator `{name}`"), span)),
        }
    }

    /// A method of a namespace like `channel` or `log`.
    pub(super) fn namespace_call(
        &mut self,
        namespace: &str,
        name: &str,
        arguments: Vec<Value>,
        span: Span,
    ) -> Result<Value, Error> {
        let result = self.host.namespace_method(namespace, name, arguments);
        match result.map_err(|error| error.or_at(span))? {
            Some(value) => Ok(value),
            None => Err(unsupported(format!("`{namespace}.{name}`"), span)),
        }
    }
}

/// The script and name an included name refers to, following links; the name itself for a
/// name of the script.
fn resolve(module: &Rc<Module>, name: &str) -> (Rc<Module>, Rc<str>) {
    let link = module.links.borrow().get(name).cloned();
    match link {
        Some(link) => resolve(&link.module, &link.name),
        None => (module.clone(), name.into()),
    }
}
