//! The build phase of a run: calls of processes and workflows, operators on channels, pipes,
//! and `.out`. The host wires the network; workflow bodies run here.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use nextflow_parser::Span;

use super::statement::Completion;
use super::{Error, Frame, Interpreter, thrown, unsupported};
use crate::exception::Exception;
use crate::lowered::{Input, Module, Place, Process, Workflow};
use crate::value::{ChannelOut, DefinitionRef, Key, Value};

impl Interpreter<'_> {
    /// Runs the named workflow selected by `-entry`, including an imported workflow alias; the
    /// result is what it emits.
    pub fn run_named_entry(&mut self, module: &Rc<Module>, name: &str) -> Result<Value, Error> {
        self.types = module.types;
        let (target, target_name) = resolve(module, name);
        if !target
            .workflows
            .iter()
            .any(|workflow| workflow.name.as_deref() == Some(&*target_name))
        {
            return Err(thrown(
                Exception::new(
                    "AbortOperationException",
                    format!("Unknown workflow entry name: {name}"),
                ),
                Span::default(),
            ));
        }
        self.call_definition(module, (name, false), Vec::new(), Span::default())
    }

    /// Runs the entry workflow of a script, which builds the dataflow network. As in Nextflow,
    /// a script with named workflows only fails, and one without workflows and processes, like
    /// an empty script, does nothing. The result is what the workflow emits.
    pub fn run_entry(&mut self, module: &Rc<Module>) -> Result<Value, Error> {
        self.types = module.types;
        let entry = module
            .workflows
            .iter()
            .find(|workflow| workflow.name.is_none())
            .cloned();
        match entry {
            Some(workflow) => self.call_workflow(module, &workflow, Vec::new(), workflow.span),
            None if !module.workflows.is_empty() => {
                let message = "No entry workflow specified";
                let exception = Exception::new("AbortOperationException", message);
                Err(thrown(exception, Span::default()))
            }
            None if module.processes.is_empty() => Ok(Value::Null),
            None => self.run_process_entry(module).map(|()| Value::Null),
        }
    }

    /// Nextflow's `ProcessEntryHandler`: a script with processes but no workflow runs its
    /// first process with inputs from the params, and publishes its outputs.
    fn run_process_entry(&mut self, module: &Rc<Module>) -> Result<(), Error> {
        let process = module.processes[0].clone();
        let params = self.params_of(module).unwrap_or(Value::Null);
        let mut arguments = Vec::new();
        for input in &process.inputs {
            match self.entry_argument(&process, input, &params) {
                Ok(argument) => arguments.push(argument),
                Err(error) => {
                    let message = error
                        .exception()
                        .map_or(String::new(), |e| e.message.clone());
                    let name = &process.name;
                    let text =
                        format!("Failed to get input arguments for process {name}: {message}");
                    self.namespace_call("log", "error", vec![Value::string(text)], process.span)?;
                    return Err(error);
                }
            }
        }
        let call = (&*process.name, false);
        let outputs = self.call_definition(module, call, arguments, process.span)?;
        let out = match outputs {
            Value::ChannelOut(out) => out,
            // A typed process gives its one output channel, or a record of them.
            Value::Channel(_) => Rc::new(ChannelOut {
                values: vec![outputs],
                names: Vec::new(),
            }),
            Value::Map(map) => {
                let entries = map.borrow().entries.clone();
                Rc::new(ChannelOut {
                    names: entries
                        .keys()
                        .map(|key| Some(key.0.to_string().into()))
                        .collect(),
                    values: entries.into_values().collect(),
                })
            }
            _ => return Ok(()),
        };
        let mut names: Vec<Rc<str>> = process
            .outputs
            .iter()
            .filter_map(|output| output.emit.clone())
            .collect();
        if process.outputs.len() == 1 && names.is_empty() {
            names.push("$out".into());
        }
        for name in names {
            let value = match out.values.as_slice() {
                [single] if process.outputs.len() == 1 => single.clone(),
                values => {
                    let index = out
                        .names
                        .iter()
                        .position(|other| other.as_deref() == Some(&*name));
                    match index.and_then(|index| values.get(index)) {
                        Some(value) => value.clone(),
                        None => continue,
                    }
                }
            };
            let options = self.output_options(module, &name)?;
            self.host
                .publish(&name, value, options)
                .map_err(|error| error.or_at(process.span))?;
        }
        Ok(())
    }

    /// The argument of the implicit process entry for one input, from the param of its name:
    /// a value as it is, a path as a file or a list of files by commas.
    fn entry_argument(
        &mut self,
        process: &Process,
        input: &Input,
        params: &Value,
    ) -> Result<Value, Error> {
        let span = process.span;
        let name_of = |slot: Option<usize>| {
            let found = process
                .variables
                .iter()
                .find(|(_, other)| Some(*other) == slot);
            found.map_or(String::new(), |(name, _)| name.to_string())
        };
        let param = |name: &str| match params {
            Value::Map(map) => (map.borrow().entries)
                .get(&Key(Value::string(name)))
                .cloned()
                .filter(|value| !matches!(value, Value::Null)),
            _ => None,
        };
        let refused = |message: String| Err(thrown(Exception::illegal_argument(message), span));
        match input {
            Input::Value { slot } => {
                let name = name_of(Some(*slot));
                match param(&name) {
                    Some(value) => Ok(value),
                    None => refused(format!("Missing required parameter: --{name}")),
                }
            }
            Input::Path { slot, .. } => {
                let name = name_of(*slot);
                let Some(value) = param(&name) else {
                    let message =
                        format!("Path input '--{name}' not provided, defaulting to empty list");
                    self.namespace_call("log", "warn", vec![Value::string(message)], span)?;
                    return Ok(Value::list(Vec::new()));
                };
                let text = value.to_string();
                if !text.contains(',') {
                    return self.call_builtin("file", vec![Value::string(text)], span);
                }
                let mut files = Vec::new();
                for part in text
                    .split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                {
                    files.push(self.call_builtin("file", vec![Value::string(part)], span)?);
                }
                Ok(Value::list(files))
            }
            Input::Env(name) if param(name).is_none() => {
                refused(format!("Missing required parameter: --{name}"))
            }
            Input::Env(_) => refused(
                "Process `env` input qualifier is not supported by implicit process entry".into(),
            ),
            Input::Stdin => refused(
                "Process `stdin` input qualifier is not supported by implicit process entry".into(),
            ),
            Input::Tuple(inputs) => {
                let mut items = Vec::new();
                for inner in inputs {
                    items.push(self.entry_argument(process, inner, params)?);
                }
                Ok(Value::list(items))
            }
            Input::Each(inner) => self.entry_argument(process, inner, params),
            Input::Record(fields) => {
                let mut entries = Vec::new();
                for (name, inner) in fields {
                    let value = self.entry_argument(process, inner, params)?;
                    entries.push((Value::String(name.clone()), value));
                }
                Ok(Value::map(entries))
            }
        }
    }

    /// Calls a process, workflow or included function by its name in a script.
    pub(super) fn call_definition(
        &mut self,
        module: &Rc<Module>,
        (name, reflective): (&str, bool),
        arguments: Vec<Value>,
        span: Span,
    ) -> Result<Value, Error> {
        let (target, target_name) = resolve(module, name);
        if let Some(plugin) = &target.plugin {
            let result = self.host.plugin_function(plugin, &target_name, arguments);
            let result = result.map_err(|error| error.or_at(span));
            // A call that matches no signature fails before the call by reflection.
            let unmatched = |error: &Error| {
                (error.exception()).is_some_and(|e| e.class == "MissingMethodException")
            };
            return match reflective {
                true => result.map_err(|error| match unmatched(&error) {
                    true => error,
                    false => super::call::invoked(error),
                }),
                false => result,
            };
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
            (Some(_), _) if !self.invocations.insert(qualified.clone()) => {
                let message = format!(
                    "Process '{name}' has been already used -- If you need to reuse the same \
                     component, include it with a different name or include it in a different \
                     workflow context"
                );
                let error = Exception::new("DuplicateProcessInvocation", message);
                return Err(thrown(error, span));
            }
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
                let outputs = self
                    .host
                    .call_process(&target, &process, &qualified, values);
                let outputs = outputs.map_err(|error| error.or_at(span))?;
                match target.types {
                    true => typed_result(outputs),
                    false => outputs,
                }
            }
            (None, Some(workflow)) => {
                self.workflow_path.push(name.into());
                let result = self.call_workflow(&target, &workflow, arguments, span);
                self.workflow_path.pop();
                match target.types {
                    true => typed_result(result?),
                    false => result?,
                }
            }
            (None, None) => {
                let call = (&*target_name, reflective);
                return self.call_script_function(&target, call, arguments, span);
            }
        };
        let key = (Rc::as_ptr(module) as usize, Rc::from(name));
        self.outputs.insert(key, outputs.clone());
        Ok(outputs)
    }

    /// Runs a workflow body with the typed operators of its own script, as Nextflow does.
    fn call_workflow(
        &mut self,
        module: &Rc<Module>,
        workflow: &Rc<Workflow>,
        arguments: Vec<Value>,
        span: Span,
    ) -> Result<Value, Error> {
        let outer = std::mem::replace(&mut self.types, module.types);
        let result = self.workflow_body(module, workflow, arguments, span);
        self.types = outer;
        result
    }

    /// Runs a workflow body with its takes bound; the result is its emits. A plain value
    /// that a workflow emits becomes a value channel.
    fn workflow_body(
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
            let options = self.output_options(module, name)?;
            self.host
                .publish(name, value, options)
                .map_err(|error| error.or_at(expression.span))?;
        }
        let mut values = Vec::new();
        let mut names = Vec::new();
        for (index, (emit, expression)) in workflow.emits.iter().enumerate() {
            // After a `return`, Nextflow finds only the emits that name a variable the body
            // has set, or a take of the emit's name.
            let bare = workflow.bare_emits.get(index).copied().unwrap_or(false);
            let take = workflow.takes.iter().find(|(name, _)| name == emit);
            let value = match take {
                _ if !returned || bare => self.expression(&mut frame, expression)?,
                Some((_, slot)) => frame.get(Place::Local(*slot)),
                None => Value::Null,
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
                // Nextflow binds a plain value at once, even `null`.
                plain => match self.host.bound_channel(plain.clone()) {
                    Some(channel) => channel,
                    None => {
                        self.namespace_call("channel", "value", vec![plain], expression.span)?
                    }
                },
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
                self.call_definition(
                    &definition.module,
                    (&definition.name, true),
                    arguments,
                    span,
                )
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
        // Typed workflows collect without flattening and combine without spreading lists.
        let name = match (self.types, name, arguments.as_slice()) {
            (true, "collect", []) => "toList",
            (true, "combine", _) => "typedCombine",
            (true, "reduce", [Value::Closure(_)]) => "typedReduce",
            _ => name,
        };
        if self.types && matches!(name, "set" | "tap") {
            let message = format!(
                "Operator `{name}` is not supported in typed workflows -- use an assignment instead"
            );
            return Err(thrown(
                Exception::new("ScriptRuntimeException", message),
                span,
            ));
        }
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
            // Typed workflows wrap channels in `ChannelImpl` and `ValueImpl`.
            Some(Value::Class(class)) if self.types && name == "getClass" => Ok(Value::Class(
                match &*class {
                    "groovyx.gpars.dataflow.DataflowVariable" => "nextflow.dataflow.ValueImpl",
                    _ => "nextflow.dataflow.ChannelImpl",
                }
                .into(),
            )),
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
        // The Java runtime of `Runtime.runtime`, whose processors are those of this machine.
        if namespace == "Runtime.runtime" && name == "availableProcessors" && arguments.is_empty() {
            let processors = std::thread::available_parallelism().map_or(1, usize::from);
            return Ok(Value::integer(processors as i32));
        }
        if namespace == "secrets" && matches!(name, "getProperty" | "getAt") && arguments.len() == 1
        {
            return self.property(
                &Value::Namespace(namespace.into()),
                &arguments[0].to_string(),
                span,
            );
        }
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

/// What a call of a typed process or workflow gives, as Nextflow's `normalizeV2`: its one
/// channel, else a record of the channels by name.
fn typed_result(outputs: Value) -> Value {
    let Value::ChannelOut(out) = &outputs else {
        return outputs;
    };
    if let [single] = out.values.as_slice() {
        return single.clone();
    }
    let entries = (out.names.iter().zip(&out.values))
        .map(|(name, value)| (Value::string(name.as_deref().unwrap_or("")), value.clone()));
    let record = Value::map(entries);
    if let Value::Map(map) = &record {
        map.borrow_mut().kind = crate::value::MapKind::Record;
    }
    record
}
