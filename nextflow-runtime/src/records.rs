//! Operators of typed workflows that Nextflow 26.04.6 changes: `groupBy`, `join` of records by
//! a field, and `combine` that only spreads tuples or adds fields to records.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use indexmap::IndexMap;
use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::{ChannelId, Key, List, ListKind, MapKind, Value};

use crate::channel::Message;
use crate::network::{Behaviour, Context, State};

/// `groupBy`: groups `(key, value)` or `(key, size, value)` tuples by key; a group with a
/// size goes out when it is full, the others at the end, each as `(key, values)`.
pub(crate) struct GroupBy {
    pub output: ChannelId,
    pub groups: IndexMap<Key, Vec<Value>>,
    pub sizes: HashMap<Key, i64>,
    pub emitted: HashSet<Key>,
}

impl Behaviour for GroupBy {
    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        while let Some(message) = context.read(0) {
            let Message::Value(tuple) = message else {
                for (key, values) in std::mem::take(&mut self.groups) {
                    if self.sizes.get(&key).is_some_and(|size| *size != -1) {
                        let message = format!(
                            "Operator `groupBy` received too few values for grouping keys: {}",
                            key.0
                        );
                        return Err(failure(message));
                    }
                    context.emit(self.output, tuple_of(vec![key.0, Value::list(values)]));
                }
                context.close(self.output);
                return Ok(State::Stopped);
            };
            let parts = match &tuple {
                Value::List(list) => list.borrow().items.clone(),
                _ => Vec::new(),
            };
            let (key, size, value) = match parts.as_slice() {
                [key, value] => (key.clone(), -1, value.clone()),
                [key, Value::Number(size), value] => (key.clone(), size.to_i64(), value.clone()),
                _ => {
                    let message = format!(
                        "Operator `groupBy` expected a 3-tuple of (key, size, value) or a 2-tuple \
                         of (key, value) but received: {tuple}"
                    );
                    return Err(failure(message));
                }
            };
            let key = Key(key);
            if self.emitted.contains(&key) {
                let message = format!(
                    "Operator `groupBy` received too many values for grouping key: {} (expected \
                     {size})",
                    key.0
                );
                return Err(failure(message));
            }
            match self.sizes.get(&key) {
                Some(known) if *known != size => {
                    let message = format!(
                        "Operator `groupBy` received inconsistent group size for key {} -- {size} \
                         != {known}\n",
                        key.0
                    );
                    return Err(failure(message));
                }
                Some(_) => {}
                None => {
                    self.sizes.insert(key.clone(), size);
                }
            }
            let group = self.groups.entry(key.clone()).or_default();
            group.push(value);
            if group.len() as i64 == size {
                let values = self.groups.shift_remove(&key).unwrap_or_default();
                context.emit(
                    self.output,
                    tuple_of(vec![key.0.clone(), Value::list(values)]),
                );
                self.emitted.insert(key);
            }
        }
        Ok(State::Running)
    }
}

/// `join(by: 'field')` of records: each pair of a left and a right record with an equal field
/// as one record; with `remainder`, the records without a partner at the end.
pub(crate) struct RecordJoin {
    pub output: ChannelId,
    pub by: Rc<str>,
    pub remainder: bool,
    pub seen: [IndexMap<Key, Vec<Value>>; 2],
    pub emitted: HashSet<Key>,
    pub done: [bool; 2],
}

impl Behaviour for RecordJoin {
    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        for side in 0..2 {
            while !self.done[side] {
                match context.read(side) {
                    Some(Message::Value(value)) => self.take(context, side, value)?,
                    Some(Message::Stop) => self.done[side] = true,
                    None => break,
                }
            }
        }
        if !(self.done[0] && self.done[1]) {
            return Ok(State::Running);
        }
        if self.remainder {
            for side in self.seen.clone() {
                for (key, values) in side {
                    if !self.emitted.contains(&key) {
                        values
                            .into_iter()
                            .for_each(|value| context.emit(self.output, value));
                    }
                }
            }
        }
        context.close(self.output);
        Ok(State::Stopped)
    }
}

impl RecordJoin {
    fn take(&mut self, context: &mut Context, side: usize, value: Value) -> Result<(), Error> {
        let field = match &value {
            Value::Map(map) if map.borrow().kind == MapKind::Record => (map.borrow().entries)
                .get(&Key(Value::String(self.by.clone())))
                .cloned()
                .unwrap_or(Value::Null),
            other => {
                let message = format!(
                    "Operator `join` expected a record but received: {other} [{}]",
                    other.class_name()
                );
                return Err(failure(message));
            }
        };
        let key = Key(field);
        let others = self.seen[1 - side].get(&key).cloned().unwrap_or_default();
        for other in &others {
            let (left, right) = if side == 0 {
                (&value, other)
            } else {
                (other, &value)
            };
            context.emit(self.output, merged(left, right));
        }
        if !others.is_empty() {
            self.emitted.insert(key.clone());
        }
        self.seen[side].entry(key).or_default().push(value);
        Ok(())
    }
}

/// `combine` of typed workflows: every pair of a left and a right value, where only tuples
/// spread into the result; or with named arguments, each record with these fields added.
pub(crate) struct TypedCombine {
    pub output: ChannelId,
    pub fields: Option<Vec<(Value, Value)>>,
    pub seen: [Vec<Value>; 2],
    pub done: [bool; 2],
}

impl Behaviour for TypedCombine {
    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        if let Some(fields) = &self.fields {
            while let Some(message) = context.read(0) {
                let Message::Value(record) = message else {
                    context.close(self.output);
                    return Ok(State::Stopped);
                };
                if !matches!(&record, Value::Map(map) if map.borrow().kind == MapKind::Record) {
                    let message = format!(
                        "Operator `combine` with named arguments expected a channel of records but \
                         received: {record} [{}]",
                        record.class_name()
                    );
                    return Err(failure(message));
                }
                let added = record_of(fields.clone());
                context.emit(self.output, merged(&record, &added));
            }
            return Ok(State::Running);
        }
        for side in 0..context.input_count() {
            while !self.done[side] {
                match context.read(side) {
                    Some(Message::Value(value)) => {
                        for other in &self.seen[1 - side] {
                            let (left, right) = if side == 0 {
                                (&value, other)
                            } else {
                                (other, &value)
                            };
                            context.emit(self.output, spread_pair(left, right));
                        }
                        self.seen[side].push(value);
                    }
                    Some(Message::Stop) => self.done[side] = true,
                    None => break,
                }
            }
        }
        if self.done[0] && self.done[1] {
            context.close(self.output);
            return Ok(State::Stopped);
        }
        Ok(State::Running)
    }
}

/// A tuple of the left and right values, where a tuple spreads into its items.
fn spread_pair(left: &Value, right: &Value) -> Value {
    let mut items = Vec::new();
    for value in [left, right] {
        match value {
            Value::List(list) if list.borrow().kind == ListKind::Tuple => {
                items.extend(list.borrow().items.iter().cloned());
            }
            other => items.push(other.clone()),
        }
    }
    tuple_of(items)
}

/// The fields of the left record with those of the right one, as `RecordMap.plus` gives them.
fn merged(left: &Value, right: &Value) -> Value {
    let mut fields = Vec::new();
    for value in [left, right] {
        if let Value::Map(map) = value {
            fields.extend(
                map.borrow()
                    .entries
                    .iter()
                    .map(|(key, value)| (key.0.clone(), value.clone())),
            );
        }
    }
    record_of(fields)
}

fn record_of(fields: Vec<(Value, Value)>) -> Value {
    let record = Value::map(fields);
    if let Value::Map(map) = &record {
        map.borrow_mut().kind = MapKind::Record;
    }
    record
}

fn tuple_of(items: Vec<Value>) -> Value {
    Value::List(Rc::new(RefCell::new(List::new(ListKind::Tuple, items))))
}

/// Nextflow's `ScriptRuntimeException`.
fn failure(message: String) -> Error {
    Error::Thrown {
        exception: Rc::new(Exception::new("ScriptRuntimeException", message)),
        span: Default::default(),
    }
}
