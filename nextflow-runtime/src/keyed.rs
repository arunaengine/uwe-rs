//! Operators that group or pair values by a key: `groupTuple`, `join`, `combine` and `cross`
//! (channels spec, sections 5.7, 5.11, 5.18 and 5.20).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use indexmap::{IndexMap, IndexSet};
use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::{ChannelId, Closure, Key, Value};

use crate::channel::Message;
use crate::network::{Behaviour, Context, State};
use crate::operator::Sort;

/// The elements of a value: a list gives its items, any other value is one element.
fn items(value: &Value) -> Vec<Value> {
    match value {
        Value::List(list) => list.borrow().items.clone(),
        other => vec![other.clone()],
    }
}

/// The key of a value at the `by` indexes, in index order. A value that is not a list is its
/// own key when `by` is `[0]`.
fn key(value: &Value, by: &[usize]) -> Result<Vec<Key>, Error> {
    match value {
        Value::List(list) => {
            let list = list.borrow();
            let mut indexes = by.to_vec();
            indexes.sort_unstable();
            indexes
                .iter()
                .map(|index| match list.items.get(*index) {
                    Some(item) => Ok(Key(string_key(item))),
                    None => Err(thrown(format!("Not a valid `by` index: {index}"))),
                })
                .collect()
        }
        other if by == [0] => Ok(vec![Key(string_key(other))]),
        _ => Err(thrown(format!("Not a valid `by` index: {by:?}"))),
    }
}

/// Key elements compare as Java does, with GStrings as Strings.
fn string_key(value: &Value) -> Value {
    match value {
        Value::GString(text) => Value::String(text.clone()),
        other => other.clone(),
    }
}

/// The key elements, then the other elements of each part, in order; one element is bare.
fn joined(by: &[usize], parts: &[&Value]) -> Value {
    let mut result: Vec<Value> = Vec::new();
    if let Some(first) = parts.first() {
        let first = items(first);
        let mut indexes = by.to_vec();
        indexes.sort_unstable();
        result.extend(
            indexes
                .iter()
                .filter_map(|index| first.get(*index).cloned()),
        );
    }
    for part in parts {
        let part = items(part);
        result.extend(
            part.into_iter()
                .enumerate()
                .filter(|(index, _)| !by.contains(index))
                .map(|(_, item)| item),
        );
    }
    match <[Value; 1]>::try_from(result) {
        Ok([single]) => single,
        Err(result) => Value::list(result),
    }
}

/// `groupTuple`: tuples with equal keys become one tuple, whose other positions are lists.
pub(crate) struct GroupTuple {
    pub output: ChannelId,
    pub by: Vec<usize>,
    /// A group is emitted as soon as it has this many tuples.
    pub size: Option<usize>,
    /// Whether incomplete groups are emitted at the end when there is a size.
    pub remainder: bool,
    /// Whether each list of a group is sorted before it is emitted.
    pub sort: Sort,
    /// The open groups in first-seen key order, with their number of tuples and their size.
    pub groups: IndexMap<Vec<Key>, (Vec<Value>, usize, Option<usize>)>,
}

impl Behaviour for GroupTuple {
    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        while let Some(message) = context.read(0) {
            let Message::Value(value) = message else {
                for (_, (group, count, size)) in std::mem::take(&mut self.groups) {
                    let complete = size.is_none_or(|size| count == size);
                    if complete || self.remainder {
                        let value = self.group_value(context, group)?;
                        context.emit(self.output, value);
                    }
                }
                context.close(self.output);
                return Ok(State::Stopped);
            };
            let Value::List(_) = value else {
                let message = "Unknown method invocation `collect` on GroupTupleOp type";
                return Err(thrown(message.into()));
            };
            let tuple = items(&value);
            let key = key(&value, &self.by)?;
            let by = self.by.clone();
            // Without a `size` option, a group key from `groupKey` gives the size.
            let size = self.size.or_else(|| match by.as_slice() {
                [index] => match tuple.get(*index) {
                    Some(Value::Object(object)) => match &object.borrow().state {
                        nextflow_evaluator::value::State::GroupKey(_, size) if *size > 0 => {
                            Some(*size as usize)
                        }
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            });
            let (group, count, size) = self.groups.entry(key.clone()).or_insert_with(|| {
                let group = (0..tuple.len())
                    .map(|index| match by.contains(&index) {
                        true => string_key(&tuple[index]),
                        false => Value::list(Vec::new()),
                    })
                    .collect();
                (group, 0, size)
            });
            for (index, item) in tuple.into_iter().enumerate() {
                if let (false, Some(Value::List(bag))) = (by.contains(&index), group.get(index)) {
                    bag.borrow_mut().items.push(item);
                }
            }
            *count += 1;
            if *size == Some(*count)
                && let Some((group, _, _)) = self.groups.shift_remove(&key)
            {
                let value = self.group_value(context, group)?;
                context.emit(self.output, value);
            }
        }
        Ok(State::Running)
    }
}

impl GroupTuple {
    fn group_value(&self, context: &mut Context, group: Vec<Value>) -> Result<Value, Error> {
        if !matches!(self.sort, Sort::None) {
            for item in &group {
                if let Value::List(list) = item {
                    let items = list.borrow().items.clone();
                    let items = self.sort.apply(context, items)?;
                    list.borrow_mut().items = items;
                }
            }
        }
        Ok(Value::list(group))
    }
}

/// `join`: pairs values of two channels with equal keys, first come first paired.
pub(crate) struct Join {
    pub output: ChannelId,
    pub by: Vec<usize>,
    pub remainder: bool,
    pub fail_on_duplicate: bool,
    pub fail_on_mismatch: bool,
    pub matched: IndexSet<Vec<Key>>,
    /// Values waiting for a partner, by key, for the left and right side.
    pub pending: [IndexMap<Vec<Key>, Vec<Value>>; 2],
    /// The keys of both sides in first-seen order, which is the order of the remainder.
    pub keys: IndexSet<Vec<Key>>,
    pub done: [bool; 2],
    /// For each side, whether a value had no elements besides its key; `None` before the
    /// first value.
    pub singleton: [Option<bool>; 2],
}

impl Behaviour for Join {
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
        for (side, pending) in self.pending.iter().enumerate() {
            let unmatched: Vec<_> = pending
                .iter()
                .filter(|(_, values)| !values.is_empty())
                .map(|(key, _)| key)
                .collect();
            if self.fail_on_duplicate && unmatched.iter().any(|key| self.matched.contains(*key)) {
                context.close(self.output);
                return Err(thrown(format!(
                    "Detected join duplicate key on side {}",
                    side + 1
                )));
            }
            if self.fail_on_mismatch && !unmatched.is_empty() {
                context.close(self.output);
                return Err(thrown(format!(
                    "Join mismatch on side {}: {:?}",
                    side + 1,
                    unmatched.into_iter().take(10).collect::<Vec<_>>()
                )));
            }
        }
        if self.remainder {
            for key in std::mem::take(&mut self.keys) {
                self.emit_remainder(context, &key);
            }
        }
        context.close(self.output);
        Ok(State::Stopped)
    }
}

impl Join {
    fn take(&mut self, context: &mut Context, side: usize, value: Value) -> Result<(), Error> {
        let key = key(&value, &self.by)?;
        if self.fail_on_duplicate && self.matched.contains(&key) {
            context.close(self.output);
            return Err(thrown(format!("Detected join duplicate key: {key:?}")));
        }
        let alone = items(&value).len() <= self.by.len();
        self.singleton[side] = Some(self.singleton[side].unwrap_or(false) || alone);
        self.keys.insert(key.clone());
        let other = 1 - side;
        let partner = self.pending[other]
            .get_mut(&key)
            .and_then(|values| (!values.is_empty()).then(|| values.remove(0)));
        match partner {
            Some(partner) => {
                if self.fail_on_duplicate {
                    self.matched.insert(key);
                }
                let (left, right) = if side == 0 {
                    (&value, &partner)
                } else {
                    (&partner, &value)
                };
                context.emit(self.output, joined(&self.by, &[left, right]));
            }
            None => self.pending[side].entry(key).or_default().push(value),
        }
        Ok(())
    }

    /// Whether a side gives no elements besides the key, as Nextflow decides it: a side
    /// without values follows the other side.
    fn singleton(&self, side: usize) -> bool {
        self.singleton[side]
            .or(self.singleton[1 - side])
            .unwrap_or(false)
    }

    /// Emits the values of a key without a partner, as Nextflow's `JoinOp.checkRemainder`:
    /// the key, then for each side its other elements or `null`, unless the side gives no
    /// elements besides the key. When neither side does, the value is the first key element.
    fn emit_remainder(&mut self, context: &mut Context, key: &Vec<Key>) {
        let mut indexes = self.by.clone();
        indexes.sort_unstable();
        loop {
            let mut result: Vec<Value> = Vec::new();
            let mut parts: Vec<Value> = Vec::new();
            let mut filled = false;
            for side in 0..2 {
                let taken = self.pending[side]
                    .get_mut(key)
                    .and_then(|values| (!values.is_empty()).then(|| values.remove(0)));
                match taken {
                    Some(value) => {
                        let elements = items(&value);
                        if !filled {
                            result.extend(
                                indexes
                                    .iter()
                                    .filter_map(|index| elements.get(*index).cloned()),
                            );
                        }
                        parts.extend(
                            elements
                                .into_iter()
                                .enumerate()
                                .filter(|(index, _)| !self.by.contains(index))
                                .map(|(_, item)| item),
                        );
                        filled = true;
                    }
                    None if !self.singleton(side) => parts.push(Value::Null),
                    None => {}
                }
            }
            if !filled {
                return;
            }
            result.extend(parts);
            let both = self.singleton[0] != Some(false) && self.singleton[1] != Some(false);
            let value = if both {
                result.into_iter().next().unwrap_or(Value::Null)
            } else {
                Value::list(result)
            };
            context.emit(self.output, value);
        }
    }
}

/// `combine`: every pair of a left and a right value, with equal keys when `by` is given.
pub(crate) struct Combine {
    pub output: ChannelId,
    pub by: Option<Vec<usize>>,
    /// The values of each side so far; the right side may be a list given at the call.
    pub seen: [Vec<Value>; 2],
    pub done: [bool; 2],
}

impl Behaviour for Combine {
    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        for side in 0..context.input_count() {
            while !self.done[side] {
                match context.read(side) {
                    Some(Message::Value(value)) => {
                        for other in self.seen[1 - side].clone() {
                            let (left, right) = if side == 0 {
                                (&value, &other)
                            } else {
                                (&other, &value)
                            };
                            if let Some(pair) = self.pair(left, right)? {
                                context.emit(self.output, pair);
                            }
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

impl Combine {
    fn pair(&self, left: &Value, right: &Value) -> Result<Option<Value>, Error> {
        match &self.by {
            None => Ok(Some(joined(&[], &[left, right]))),
            Some(by) if key(left, by)? == key(right, by)? => Ok(Some(joined(by, &[left, right]))),
            Some(_) => Ok(None),
        }
    }
}

/// `cross`: pairs each right value with the first left value of its key, as `[left, right]`.
/// Nextflow keeps the latest left value, but its threads usually pair the right values
/// before later left values come, so the first left value is what its runs give.
pub(crate) struct Cross {
    pub output: ChannelId,
    pub key: Option<Rc<Closure>>,
    pub left: IndexMap<Key, Value>,
    /// Right values that came before any left value of their key.
    pub waiting: IndexMap<Key, Vec<Value>>,
    pub done: [bool; 2],
}

impl Behaviour for Cross {
    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        for side in 0..2 {
            while !self.done[side] {
                let value = match context.read(side) {
                    Some(Message::Value(value)) => value,
                    Some(Message::Stop) => {
                        self.done[side] = true;
                        continue;
                    }
                    None => break,
                };
                let key = Key(match &self.key {
                    Some(closure) => context
                        .interpreter
                        .call_closure(closure, vec![value.clone()])?,
                    None => cross_key(&value),
                });
                if side == 0 {
                    for right in self.waiting.shift_remove(&key).unwrap_or_default() {
                        context.emit(self.output, Value::list(vec![value.clone(), right]));
                    }
                    self.left.entry(key).or_insert(value);
                } else if let Some(left) = self.left.get(&key) {
                    context.emit(self.output, Value::list(vec![left.clone(), value]));
                } else {
                    self.waiting.entry(key).or_default().push(value);
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

/// The default key of `cross`: the first element of a list, the first value of a map, the
/// key of a map entry, or the value itself.
fn cross_key(value: &Value) -> Value {
    match value {
        Value::List(list) => list.borrow().items.first().cloned().unwrap_or(Value::Null),
        Value::Map(map) => map
            .borrow()
            .entries
            .values()
            .next()
            .cloned()
            .unwrap_or(Value::Null),
        Value::MapEntry(entry) => entry.0.clone(),
        other => other.clone(),
    }
}

fn thrown(message: String) -> Error {
    Error::Thrown {
        exception: Rc::new(Exception::illegal_argument(message)),
        span: Default::default(),
    }
}
