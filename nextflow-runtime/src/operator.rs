//! Channel factories and the first operators, with the rules of the channels spec, section 5.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::{ChannelId, Closure, Value};

use crate::channel::Message;
use crate::network::{Behaviour, Context, State};

/// `channel.of`, `channel.fromList` and `channel.empty`: emits its values at ignition, then
/// closes. `channel.value(v)` binds its value channel; without a value it stays unbound.
pub(crate) struct Factory {
    pub values: Vec<Value>,
    pub output: ChannelId,
    pub close: bool,
}

impl Behaviour for Factory {
    fn ignite(&mut self, context: &mut Context) -> Result<State, Error> {
        for value in std::mem::take(&mut self.values) {
            context.emit(self.output, value);
        }
        if self.close {
            context.close(self.output);
        }
        Ok(State::Stopped)
    }

    fn run(&mut self, _: &mut Context) -> Result<State, Error> {
        Ok(State::Stopped)
    }
}

/// The operators that read one channel and handle each value on its own.
pub(crate) enum Each {
    /// `map`: the closure's result, unless it is `null`.
    Map(Rc<Closure>),
    /// `filter`: values for which the closure is true, or that the criteria's `isCase` accepts.
    Filter(Value),
    /// `flatMap`: the elements of the closure's result.
    FlatMap(Option<Rc<Closure>>),
    /// `flatten`: the leaves of nested collections.
    Flatten,
    /// `view`: prints each value, or the closure's result, and forwards the value.
    View {
        closure: Option<Rc<Closure>>,
        new_line: bool,
    },
}

pub(crate) struct Single {
    pub kind: Each,
    pub output: ChannelId,
}

impl Behaviour for Single {
    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        while let Some(message) = context.read(0) {
            let Message::Value(value) = message else {
                context.close(self.output);
                return Ok(State::Stopped);
            };
            match &self.kind {
                Each::Map(closure) => {
                    let result = context.interpreter.call_closure(closure, vec![value])?;
                    if !matches!(result, Value::Null) {
                        context.emit(self.output, result);
                    }
                }
                Each::Filter(criteria) => {
                    let passes = match criteria {
                        Value::Closure(closure) => context
                            .interpreter
                            .call_closure(closure, vec![value.clone()])?
                            .truth(),
                        other => other.is_case(&value).map_err(thrown)?,
                    };
                    if passes {
                        context.emit(self.output, value);
                    }
                }
                Each::FlatMap(closure) => {
                    let result = match closure {
                        Some(closure) => context.interpreter.call_closure(closure, vec![value])?,
                        None => value,
                    };
                    for item in elements(result) {
                        context.emit(self.output, item);
                    }
                }
                Each::Flatten => {
                    let mut leaves = Vec::new();
                    flatten(value, &mut leaves);
                    for leaf in leaves {
                        context.emit(self.output, leaf);
                    }
                }
                Each::View { closure, new_line } => {
                    let shown = match closure {
                        Some(closure) => context
                            .interpreter
                            .call_closure(closure, vec![value.clone()])?,
                        None => value.clone(),
                    };
                    let end = if *new_line { "\n" } else { "" };
                    context.interpreter.print(&format!("{shown}{end}"));
                    context.emit(self.output, value);
                }
            }
        }
        Ok(State::Running)
    }
}

/// The operators that wait for the end of their source: `collect`, `toList` and `ifEmpty`.
pub(crate) enum Gather {
    /// `collect([closure], flat: bool)`: a list, or an empty value when nothing came.
    Collect {
        closure: Option<Rc<Closure>>,
        flat: bool,
    },
    /// `toList()`: a list, `[]` when nothing came.
    ToList,
    /// `ifEmpty(value)`: forwards the values, or emits the default when there were none.
    IfEmpty(Value),
    /// `first([criteria])`: the first value that passes; unbound when none does.
    First(Option<Value>),
}

pub(crate) struct Gathering {
    pub kind: Gather,
    pub output: ChannelId,
    pub items: Vec<Value>,
    pub done: bool,
}

impl Behaviour for Gathering {
    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        while let Some(message) = context.read(0) {
            let Message::Value(value) = message else {
                return self.finish(context);
            };
            match &self.kind {
                Gather::Collect { closure, flat } => {
                    let item = match closure {
                        Some(closure) => context.interpreter.call_closure(closure, vec![value])?,
                        None => value,
                    };
                    match (&item, flat) {
                        (Value::List(list), true) => self.items.extend(list.borrow().items.clone()),
                        _ => self.items.push(item),
                    }
                }
                Gather::ToList => self.items.push(value),
                Gather::IfEmpty(_) => {
                    self.items.push(Value::Null);
                    context.emit(self.output, value);
                }
                Gather::First(criteria) => {
                    if self.done {
                        continue;
                    }
                    let passes = match criteria {
                        None => true,
                        Some(Value::Closure(closure)) => context
                            .interpreter
                            .call_closure(closure, vec![value.clone()])?
                            .truth(),
                        Some(other) => other.is_case(&value).map_err(thrown)?,
                    };
                    if passes {
                        self.done = true;
                        context.emit(self.output, value);
                    }
                }
            }
        }
        Ok(State::Running)
    }
}

impl Gathering {
    fn finish(&mut self, context: &mut Context) -> Result<State, Error> {
        let items = std::mem::take(&mut self.items);
        match &self.kind {
            Gather::Collect { .. } if items.is_empty() => context.close(self.output),
            Gather::Collect { .. } | Gather::ToList => {
                context.emit(self.output, Value::list(items))
            }
            Gather::IfEmpty(default) if items.is_empty() => {
                let default = match default {
                    Value::Closure(closure) => {
                        context.interpreter.call_closure(closure, Vec::new())?
                    }
                    other => other.clone(),
                };
                context.emit(self.output, default);
            }
            Gather::IfEmpty(_) => {}
            // A `first` that found nothing without criteria is empty; with criteria it stays
            // unbound, as in Nextflow, and its readers are reported as waiting.
            Gather::First(None) if !self.done => context.close(self.output),
            Gather::First(_) => {}
        }
        if !matches!(self.kind, Gather::First(_)) {
            context.close(self.output);
        }
        Ok(State::Stopped)
    }
}

/// `mix`: every value of every input as it comes; closes when all inputs have closed.
pub(crate) struct Mix {
    pub output: ChannelId,
    pub closed: Vec<bool>,
}

impl Behaviour for Mix {
    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        for input in 0..context.input_count() {
            while !self.closed[input] {
                match context.read(input) {
                    Some(Message::Value(value)) => context.emit(self.output, value),
                    Some(Message::Stop) => self.closed[input] = true,
                    None => break,
                }
            }
        }
        if self.closed.iter().all(|closed| *closed) {
            context.close(self.output);
            return Ok(State::Stopped);
        }
        Ok(State::Running)
    }
}

/// The values `flatMap` emits for one result: the elements of a collection, the entries of
/// a map, the key and value of an entry, nothing for `null`, else the result itself.
fn elements(value: Value) -> Vec<Value> {
    match value {
        Value::Null => Vec::new(),
        Value::List(list) => list.borrow().items.clone(),
        Value::Set(set) => set
            .borrow()
            .items
            .iter()
            .map(|item| item.0.clone())
            .collect(),
        Value::Map(map) => map
            .borrow()
            .entries
            .iter()
            .map(|(key, value)| Value::MapEntry(Rc::new((key.0.clone(), value.clone()))))
            .collect(),
        Value::MapEntry(entry) => vec![entry.0.clone(), entry.1.clone()],
        other => vec![other],
    }
}

/// The leaves of nested lists and sets; maps stay whole and `null` gives nothing.
fn flatten(value: Value, leaves: &mut Vec<Value>) {
    match value {
        Value::Null => {}
        Value::List(list) => {
            for item in list.borrow().items.clone() {
                flatten(item, leaves);
            }
        }
        Value::Set(set) => {
            for item in set.borrow().items.iter() {
                flatten(item.0.clone(), leaves);
            }
        }
        other => leaves.push(other),
    }
}

fn thrown(exception: nextflow_evaluator::exception::Exception) -> Error {
    Error::Thrown {
        exception: Rc::new(exception),
        span: Default::default(),
    }
}
