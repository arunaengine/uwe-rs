//! The host of the build phase: factory and operator calls in workflow bodies become nodes.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_config::Config;
use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::lowered::{Expression, ExpressionKind, Input, Module, Process};
use nextflow_evaluator::value::{ChannelId, ChannelOut, ListKind, Value};

use crate::network::Network;
use crate::operator::{Each, Factory, Gather, Gathering, Mix, Single};
use crate::process::{ProcessNode, singleton};

pub(crate) struct Builder<'a> {
    pub network: &'a mut Network,
    pub base: &'a mut dyn Host,
    /// Whether tasks run their `stub:` section.
    pub stub: bool,
    pub config: &'a Config,
}

impl Host for Builder<'_> {
    fn print(&mut self, text: &str) {
        self.base.print(text);
    }

    fn launch_dir(&self) -> String {
        self.base.launch_dir()
    }

    fn namespace_method(
        &mut self,
        namespace: &str,
        name: &str,
        arguments: Vec<Value>,
    ) -> Result<Option<Value>, Error> {
        if namespace == "channel" && name != "fromList" && arguments.iter().any(is_channel) {
            let message = format!("Argument of `channel.{name}` cannot be a channel object");
            return Err(thrown(message));
        }
        let (values, value, close) = match (namespace, name, arguments.as_slice()) {
            ("channel", "of", _) => (expand_ranges(arguments), false, true),
            ("channel", "fromList", [list]) => (elements(list), false, true),
            ("channel", "empty", []) => (Vec::new(), false, true),
            // `channel.value()` and `channel.value(null)` are never bound.
            ("channel", "value", [] | [Value::Null]) => (Vec::new(), true, false),
            ("channel", "value", [value]) => (vec![value.clone()], true, true),
            ("log", "info" | "warn" | "error" | "debug", [message]) => {
                let prefix = match name {
                    "warn" => "WARN: ",
                    "error" => "ERROR ~ ",
                    _ => "",
                };
                if name != "debug" {
                    self.base.print(&format!("{prefix}{message}\n"));
                }
                return Ok(Some(Value::Null));
            }
            _ => return Ok(None),
        };
        let output = self.network.channel(value);
        let factory = Factory {
            values,
            output,
            close,
        };
        self.network.add(name, &[], Box::new(factory));
        Ok(Some(Value::Channel(output)))
    }

    /// A process call: each input reads its channel; a plain value becomes a bound value
    /// channel. A process without queue inputs runs one task and emits value channels.
    fn call_process(
        &mut self,
        module: &Rc<Module>,
        process: &Rc<Process>,
        name: &str,
        arguments: Vec<Value>,
    ) -> Result<Value, Error> {
        let arguments: Vec<Value> = arguments
            .into_iter()
            .flat_map(|argument| match argument {
                Value::ChannelOut(out) => out.values.clone(),
                other => vec![other],
            })
            .collect();
        if arguments.len() != process.inputs.len() {
            let message = format!(
                "Process `{name}` declares {} input channels but {} were given",
                process.inputs.len(),
                arguments.len()
            );
            return Err(thrown(message));
        }
        if process
            .inputs
            .iter()
            .any(|input| matches!(input, Input::Each(_)))
        {
            return Err(Error::Unsupported {
                message: "`each` process inputs".into(),
                span: process.span,
            });
        }
        let mut inputs = Vec::new();
        let mut queue_inputs = false;
        for argument in arguments {
            let channel = match argument {
                Value::Channel(channel) => channel,
                Value::Null => {
                    return Err(thrown("A process input channel evaluates to null".into()));
                }
                plain => {
                    let channel = self.network.channel(true);
                    self.network.bind(channel, plain);
                    channel
                }
            };
            queue_inputs |= !self.network.is_value(channel);
            inputs.push(channel);
        }
        // Selectors match the definition name, the alias and the fully qualified name.
        let alias = name.rsplit(':').next().unwrap_or(name);
        let settings = self
            .config
            .process_settings(&[&process.name, alias, name], &labels(process))
            .map_err(|exception| Error::Thrown {
                exception: Rc::new(exception),
                span: process.span,
            })?;
        let singleton = singleton(process, queue_inputs);
        let outputs: Vec<ChannelId> = process
            .outputs
            .iter()
            .map(|_| self.network.channel(singleton))
            .collect();
        let node = ProcessNode {
            module: module.clone(),
            process: process.clone(),
            name: name.into(),
            outputs: outputs.clone(),
            singleton,
            stub: self.stub,
            settings: Rc::new(settings),
            rounds: 0,
            poisoned: false,
            pending: Default::default(),
        };
        self.network.add(name, &inputs, Box::new(node));
        let values = outputs.into_iter().map(Value::Channel).collect();
        let names = process
            .outputs
            .iter()
            .map(|output| output.emit.clone())
            .collect();
        Ok(Value::ChannelOut(Rc::new(ChannelOut { values, names })))
    }

    fn operator(
        &mut self,
        source: &Value,
        name: &str,
        arguments: Vec<Value>,
    ) -> Result<Option<Value>, Error> {
        let (source, arguments) = match source {
            Value::Channel(channel) => (*channel, arguments),
            Value::ChannelOut(out) => match (out.values.as_slice(), arguments.is_empty()) {
                ([], _) => {
                    return Err(thrown(format!(
                        "Operator `{name}` cannot be applied to an undefined output"
                    )));
                }
                ([Value::Channel(channel)], _) => (*channel, arguments),
                ([Value::Channel(first), rest @ ..], true) => (*first, rest.to_vec()),
                _ => {
                    let message = format!(
                        "Multi-channel output cannot be applied to operator {name} for which argument is already provided"
                    );
                    return Err(thrown(message));
                }
            },
            _ => return Ok(None),
        };
        let same_kind = self.network.is_value(source);
        let (kind, value) = match (name, arguments.as_slice()) {
            ("map", [Value::Closure(closure)]) => {
                (Kind::Each(Each::Map(closure.clone())), same_kind)
            }
            ("filter", [criteria]) => (Kind::Each(Each::Filter(criteria.clone())), same_kind),
            ("flatMap", []) => (Kind::Each(Each::FlatMap(None)), false),
            ("flatMap", [Value::Closure(closure)]) => {
                (Kind::Each(Each::FlatMap(Some(closure.clone()))), false)
            }
            ("flatten", []) => (Kind::Each(Each::Flatten), false),
            ("view", rest) => {
                let (options, closure) = options_and_closure(rest);
                let new_line = option(&options, "newLine").is_none_or(|value| value.truth());
                (Kind::Each(Each::View { closure, new_line }), same_kind)
            }
            ("collect", rest) => {
                let (options, closure) = options_and_closure(rest);
                let flat = option(&options, "flat").is_none_or(|value| value.truth());
                (Kind::Gather(Gather::Collect { closure, flat }), true)
            }
            ("toList", []) => (Kind::Gather(Gather::ToList), true),
            ("ifEmpty", [default]) => (Kind::Gather(Gather::IfEmpty(default.clone())), same_kind),
            ("first", []) => (Kind::Gather(Gather::First(None)), true),
            ("first", [criteria]) => (Kind::Gather(Gather::First(Some(criteria.clone()))), true),
            ("mix", others) if !others.is_empty() => {
                let mut inputs = vec![source];
                for other in others {
                    inputs.extend(
                        channels(other)
                            .ok_or_else(|| thrown(format!("Invalid `mix` operand: {other}")))?,
                    );
                }
                let output = self.network.channel(false);
                let closed = vec![false; inputs.len()];
                self.network
                    .add(name, &inputs, Box::new(Mix { output, closed }));
                return Ok(Some(Value::Channel(output)));
            }
            ("mix", []) => {
                return Err(thrown(
                    "Operator 'mix' should have at least one right operand".into(),
                ));
            }
            _ => return Ok(None),
        };
        let output = self.network.channel(value);
        match kind {
            Kind::Each(kind) => {
                self.network
                    .add(name, &[source], Box::new(Single { kind, output }))
            }
            Kind::Gather(kind) => {
                let gathering = Gathering {
                    kind,
                    output,
                    items: Vec::new(),
                    done: false,
                };
                self.network.add(name, &[source], Box::new(gathering));
            }
        }
        Ok(Some(Value::Channel(output)))
    }
}

/// The labels of a process, from its `label` directives, which must be plain strings.
fn labels(process: &Process) -> Vec<String> {
    process
        .directives
        .iter()
        .filter(|directive| &*directive.name == "label")
        .filter_map(|directive| match directive.arguments.positional.first() {
            Some(Expression {
                kind: ExpressionKind::Literal(label),
                ..
            }) => Some(label.to_string()),
            _ => None,
        })
        .collect()
}

enum Kind {
    Each(Each),
    Gather(Gather),
}

/// The channels of a channel or of the outputs of a call.
fn channels(value: &Value) -> Option<Vec<ChannelId>> {
    match value {
        Value::Channel(channel) => Some(vec![*channel]),
        Value::ChannelOut(out) => out
            .values
            .iter()
            .map(|value| match value {
                Value::Channel(channel) => Some(*channel),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

fn is_channel(value: &Value) -> bool {
    matches!(value, Value::Channel(_) | Value::ChannelOut(_))
}

/// The arguments of `channel.of`: a range gives its elements, a list stays one value.
fn expand_ranges(arguments: Vec<Value>) -> Vec<Value> {
    let mut values = Vec::new();
    for argument in arguments {
        match &argument {
            Value::List(list) if list.borrow().kind == ListKind::Range => {
                values.extend(list.borrow().items.clone());
            }
            _ => values.push(argument),
        }
    }
    values
}

fn elements(value: &Value) -> Vec<Value> {
    match value {
        Value::List(list) => list.borrow().items.clone(),
        Value::Set(set) => set
            .borrow()
            .items
            .iter()
            .map(|item| item.0.clone())
            .collect(),
        other => vec![other.clone()],
    }
}

/// Named arguments, which come first as a map, and a trailing closure.
fn options_and_closure(
    arguments: &[Value],
) -> (
    Option<Value>,
    Option<Rc<nextflow_evaluator::value::Closure>>,
) {
    let mut options = None;
    let mut closure = None;
    for argument in arguments {
        match argument {
            Value::Map(_) => options = Some(argument.clone()),
            Value::Closure(value) => closure = Some(value.clone()),
            _ => {}
        }
    }
    (options, closure)
}

fn option(options: &Option<Value>, name: &str) -> Option<Value> {
    match options {
        Some(Value::Map(map)) => map
            .borrow()
            .entries
            .get(&nextflow_evaluator::value::Key(Value::string(name)))
            .cloned(),
        _ => None,
    }
}

fn thrown(message: String) -> Error {
    Error::Thrown {
        exception: Rc::new(Exception::illegal_argument(message)),
        span: Default::default(),
    }
}
