//! Operators with several outputs or ordered inputs: `branch`, `multiMap`, `concat` and
//! `merge` (channels spec, sections 5.2, 5.8, 5.25 and 5.27).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::{ChannelId, Closure, Key, Value};

use crate::channel::Message;
use crate::network::{Behaviour, Context, State};

/// `branch` and `multiMap`: the rewritten criteria closure gives `[label, value]` for
/// `branch`, and a map of values by label for `multiMap`.
pub(crate) struct Route {
    pub criteria: Rc<Closure>,
    pub labels: Vec<Value>,
    /// One output per label, in label order.
    pub outputs: Vec<ChannelId>,
    /// Whether every output gets a value (`multiMap`), not only the first that matches.
    pub all: bool,
}

impl Behaviour for Route {
    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        while let Some(message) = context.read(0) {
            let Message::Value(value) = message else {
                for output in &self.outputs {
                    context.close(*output);
                }
                return Ok(State::Stopped);
            };
            let result = context
                .interpreter
                .call_closure(&self.criteria, vec![value])?;
            if self.all {
                let Value::Map(map) = &result else {
                    continue;
                };
                for (label, output) in self.labels.iter().zip(&self.outputs) {
                    let value = map.borrow().entries.get(&Key(label.clone())).cloned();
                    if let Some(value) = value {
                        context.emit(*output, value);
                    }
                }
            } else if let Value::List(choice) = &result {
                let choice = choice.borrow().items.clone();
                if let [label, value] = choice.as_slice()
                    && let Some(index) = self.labels.iter().position(|l| l.java_equals(label))
                {
                    context.emit(self.outputs[index], value.clone());
                }
            }
        }
        Ok(State::Running)
    }
}

/// `concat`: all values of the first input, then of the second, and so on.
pub(crate) struct Concat {
    pub output: ChannelId,
    /// The input being read.
    pub current: usize,
}

impl Behaviour for Concat {
    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        while self.current < context.input_count() {
            match context.read(self.current) {
                Some(Message::Value(value)) => context.emit(self.output, value),
                Some(Message::Stop) => self.current += 1,
                None => return Ok(State::Running),
            }
        }
        context.close(self.output);
        Ok(State::Stopped)
    }
}

/// `merge`: one value of each input per round; a value channel gives its value every round.
/// The result is the closure's, or the values with lists flattened one level.
pub(crate) struct Merge {
    pub output: ChannelId,
    pub closure: Option<Rc<Closure>>,
}

impl Behaviour for Merge {
    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        loop {
            if (0..context.input_count())
                .any(|input| matches!(context.peek(input), Some(Message::Stop)))
            {
                context.close(self.output);
                return Ok(State::Stopped);
            }
            let mut values = Vec::new();
            for input in 0..context.input_count() {
                match context.peek(input) {
                    Some(Message::Value(value)) => values.push(value),
                    Some(Message::Stop) => {
                        context.close(self.output);
                        return Ok(State::Stopped);
                    }
                    None => return Ok(State::Running),
                }
            }
            for input in 0..context.input_count() {
                if !context.is_value(input) {
                    context.consume(input);
                }
            }
            let result = match &self.closure {
                Some(closure) => context.interpreter.call_closure(closure, values)?,
                None => Value::list(
                    values
                        .into_iter()
                        .flat_map(|value| match value {
                            Value::List(list) => list.borrow().items.clone(),
                            other => vec![other],
                        })
                        .collect(),
                ),
            };
            context.emit(self.output, result);
            // A first input that is a value channel gives one round.
            if context.is_value(0) {
                context.close(self.output);
                return Ok(State::Stopped);
            }
        }
    }
}
