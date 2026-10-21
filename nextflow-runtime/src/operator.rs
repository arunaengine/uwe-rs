//! Channel factories and the first operators, with the rules of the channels spec, section 5.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::{ChannelId, Closure, Value};

use crate::channel::Message;
use crate::network::{Behaviour, Context, State};
use crate::notes::spread;

/// `channel.of`, `channel.fromList` and `channel.empty`: binds its source at ignition, then
/// closes. `channel.value(v)` binds its value channel; without a value it stays unbound.
pub(crate) struct Factory {
    pub values: Vec<Value>,
    pub output: ChannelId,
    pub close: bool,
    /// Why the factory cannot give its values, like a missing file of `fromPath` with
    /// `checkIfExists`, which Nextflow reports when the factory starts, not when it is made.
    pub failure: Option<Error>,
}

impl Behaviour for Factory {
    fn ignite(&mut self, context: &mut Context) -> Result<State, Error> {
        if let Some(failure) = self.failure.take() {
            return Err(failure);
        }
        context.source(self.output, std::mem::take(&mut self.values), self.close);
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
    /// `toInteger`, `toLong`, `toFloat` and `toDouble`: each value `as` the class.
    Cast(&'static str),
    /// `view`: prints each value, or the closure's result, and forwards the value.
    View {
        closure: Option<Rc<Closure>>,
        new_line: bool,
    },
    /// `unique`: values whose key, the closure's result or the value, was not seen before.
    Unique {
        closure: Option<Rc<Closure>>,
        seen: Vec<Value>,
    },
    /// `distinct`: drops a value whose key equals the previous one, which starts as `null`.
    Distinct {
        closure: Option<Rc<Closure>>,
        previous: Value,
    },
    /// `take(n)`: the first `n` values; all of them for a negative `n`.
    Take(i64),
    /// `until`: values until the closure is true for one.
    Until(Rc<Closure>),
    /// `transpose`: a tuple with lists becomes one tuple per list element.
    Transpose {
        by: Option<Vec<usize>>,
        remainder: bool,
    },
}

pub(crate) struct Single {
    pub kind: Each,
    pub output: ChannelId,
}

impl Behaviour for Single {
    fn ignite(&mut self, context: &mut Context) -> Result<State, Error> {
        if let Each::Take(0) = self.kind {
            context.close(self.output);
            return Ok(State::Stopped);
        }
        self.run(context)
    }

    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        while let Some(message) = context.read(0) {
            let Message::Value(value) = message else {
                context.close(self.output);
                return Ok(State::Stopped);
            };
            match &mut self.kind {
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
                        spread(context.interpreter.host(), &item);
                        context.emit(self.output, item);
                    }
                }
                Each::Cast(class) => {
                    let value = context.interpreter.cast_value(value, class)?;
                    context.emit(self.output, value);
                }
                Each::Flatten => {
                    if matches!(value, Value::Null) {
                        continue;
                    }
                    let mut leaves = Vec::new();
                    flatten(value, &mut leaves);
                    for leaf in leaves {
                        spread(context.interpreter.host(), &leaf);
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
                Each::Unique { closure, seen } => {
                    let key = match closure {
                        Some(closure) => context
                            .interpreter
                            .call_closure(closure, vec![value.clone()])?,
                        None => value.clone(),
                    };
                    if !seen.iter().any(|earlier| earlier.groovy_equals(&key)) {
                        seen.push(key);
                        context.emit(self.output, value);
                    }
                }
                Each::Distinct { closure, previous } => {
                    let key = match closure {
                        Some(closure) => context
                            .interpreter
                            .call_closure(closure, vec![value.clone()])?,
                        None => value.clone(),
                    };
                    if !key.groovy_equals(previous) {
                        *previous = key;
                        context.emit(self.output, value);
                    }
                }
                Each::Take(remaining) => {
                    if *remaining != 0 {
                        context.emit(self.output, value);
                        *remaining -= 1;
                    }
                    if *remaining == 0 {
                        context.close(self.output);
                        return Ok(State::Stopped);
                    }
                }
                Each::Until(closure) => {
                    if context
                        .interpreter
                        .call_closure(closure, vec![value.clone()])?
                        .truth()
                    {
                        context.close(self.output);
                        return Ok(State::Stopped);
                    }
                    context.emit(self.output, value);
                }
                Each::Transpose { by, remainder } => {
                    spread_transposed(context.interpreter.host(), &value, by.as_deref());
                    for tuple in transposed(&value, by.as_deref(), *remainder)? {
                        context.emit(self.output, tuple);
                    }
                }
            }
        }
        Ok(State::Running)
    }
}

/// Tells the host of the files that `transpose` turns into items of their own: those at the
/// positions it transposes, or without `by` those at any position, since a file alone in a
/// simulated run may be one of a list in a real run.
fn spread_transposed(host: &mut dyn Host, value: &Value, by: Option<&[usize]>) {
    let Value::List(list) = value else {
        return;
    };
    for (index, item) in list.borrow().items.iter().enumerate() {
        if by.is_none_or(|by| by.contains(&index)) {
            spread(host, item);
        }
    }
}

/// The tuples `transpose` makes of one value: element `i` of each transposed list, with the
/// other positions unchanged. A value without lists stays as it is.
fn transposed(value: &Value, by: Option<&[usize]>, remainder: bool) -> Result<Vec<Value>, Error> {
    let Value::List(list) = value else {
        return Ok(vec![value.clone()]);
    };
    let items = list.borrow().items.clone();
    let lists: Vec<usize> = match by {
        Some(by) => by.to_vec(),
        None => (0..items.len())
            .filter(|index| matches!(items[*index], Value::List(_)))
            .collect(),
    };
    if lists.is_empty() {
        return Ok(vec![value.clone()]);
    }
    let mut columns = Vec::new();
    for index in &lists {
        match items.get(*index) {
            Some(Value::List(column)) => columns.push(column.borrow().items.clone()),
            _ => {
                return Err(thrown_text(format!(
                    "Not a valid collection at index {index}"
                )));
            }
        }
    }
    let lengths = columns.iter().map(Vec::len);
    let count = if remainder {
        lengths.max()
    } else {
        lengths.min()
    }
    .unwrap_or(0);
    Ok((0..count)
        .map(|row| {
            let mut tuple = items.clone();
            for (index, column) in lists.iter().zip(&columns) {
                tuple[*index] = column.get(row).cloned().unwrap_or(Value::Null);
            }
            Value::list(tuple)
        })
        .collect())
}

/// The operators that wait for the end of their source: `collect`, `toList` and `ifEmpty`.
pub(crate) enum Gather {
    /// `collect([closure], flat: bool, sort: bool or closure)`: a list, sorted when `sort`
    /// says so, or an empty value when nothing came.
    Collect {
        closure: Option<Rc<Closure>>,
        flat: bool,
        sort: bool,
        sort_by: Option<Rc<Closure>>,
    },
    /// `toList()`: a list, `[]` when nothing came.
    ToList,
    /// `ifEmpty(value)`: forwards the values, or emits the default when there were none.
    IfEmpty(Value),
    /// `first([criteria])`: the first value that passes; unbound when none does.
    First(Option<Value>),
    /// `toSortedList([closure])`: a sorted list, `[]` when nothing came.
    ToSortedList(Option<Rc<Closure>>),
    /// `count([criteria])`: the number of values that pass.
    Count(Option<Value>),
    /// `sum([closure])`, `min([closure])` and `max([closure])`: `null` when nothing came.
    Sum(Option<Rc<Closure>>),
    Min(Option<Rc<Closure>>),
    Max(Option<Rc<Closure>>),
    /// `reduce([seed], closure)`: the accumulator, which starts as the seed.
    Reduce(Rc<Closure>),
    /// `reduce(closure)` of typed workflows, which fails for an empty channel.
    TypedReduce(Rc<Closure>),
    /// `last()`: the last value, `null` when nothing came.
    Last,
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
                Gather::Collect { closure, flat, .. } => {
                    let item = match closure {
                        Some(closure) => context.interpreter.call_closure(closure, vec![value])?,
                        None => value,
                    };
                    match (&item, flat) {
                        (Value::List(list), true) => self.items.extend(list.borrow().items.clone()),
                        _ => self.items.push(item),
                    }
                }
                Gather::ToList | Gather::ToSortedList(_) | Gather::Min(_) | Gather::Max(_) => {
                    self.items.push(value)
                }
                Gather::Last => self.items = vec![value],
                Gather::Count(criteria) => {
                    let passes = match criteria {
                        None => true,
                        Some(Value::Closure(closure)) => context
                            .interpreter
                            .call_closure(closure, vec![value.clone()])?
                            .truth(),
                        Some(other) => other.is_case(&value).map_err(thrown)?,
                    };
                    if passes {
                        self.items.push(Value::Null);
                    }
                }
                Gather::Sum(closure) => {
                    let item = match closure {
                        Some(closure) => context.interpreter.call_closure(closure, vec![value])?,
                        None => value,
                    };
                    if matches!(item, Value::Null) {
                        continue;
                    }
                    let total = match self.items.pop() {
                        None => item,
                        Some(total) => total
                            .apply(nextflow_evaluator::value::Operator::Add, &item)
                            .map_err(thrown)?,
                    };
                    self.items.push(total);
                }
                // Typed workflows keep whatever the closure gives, null too.
                Gather::TypedReduce(closure) => {
                    let next = match self.items.pop() {
                        None => value,
                        Some(accumulator) => {
                            (context.interpreter).call_closure(closure, vec![accumulator, value])?
                        }
                    };
                    self.items.push(next);
                }
                Gather::Reduce(closure) => {
                    let accumulator = self.items.pop();
                    let next = match accumulator {
                        None | Some(Value::Null) => value,
                        Some(accumulator) => {
                            let result = context
                                .interpreter
                                .call_closure(closure, vec![accumulator.clone(), value])?;
                            if matches!(result, Value::Null) {
                                accumulator
                            } else {
                                result
                            }
                        }
                    };
                    self.items.push(next);
                }
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
            Gather::Collect {
                sort: true,
                sort_by,
                ..
            } => {
                let sort_by = sort_by.clone();
                let sorted = sorted(context, items, sort_by.as_ref())?;
                context.emit(self.output, Value::list(sorted));
            }
            Gather::Collect { .. } | Gather::ToList => {
                context.emit(self.output, Value::list(items))
            }
            Gather::ToSortedList(closure) => {
                let sorted = sorted(context, items, closure.as_ref())?;
                context.emit(self.output, Value::list(sorted));
            }
            Gather::Count(_) => {
                context.emit(self.output, Value::integer(items.len() as i32));
            }
            Gather::TypedReduce(_) if items.is_empty() => {
                let message = "Operator `reduce` received an empty channel with no initial value \
                               -- make sure to provide an initial value if the channel might be \
                               empty";
                return Err(Error::Thrown {
                    exception: Rc::new(Exception::new("ScriptRuntimeException", message)),
                    span: Default::default(),
                });
            }
            Gather::Sum(_) | Gather::Reduce(_) | Gather::TypedReduce(_) | Gather::Last => {
                context.emit(self.output, items.into_iter().next().unwrap_or(Value::Null));
            }
            Gather::Min(closure) | Gather::Max(closure) => {
                let closure = closure.clone();
                let sorted = sorted(context, items, closure.as_ref())?;
                let extreme = match self.kind {
                    Gather::Min(_) => sorted.first().cloned(),
                    _ => sorted.last().cloned(),
                };
                context.emit(self.output, extreme.unwrap_or(Value::Null));
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

/// Values sorted by natural order, by the key a one-parameter closure gives, or by a
/// two-parameter comparator closure. The sort is stable.
pub(crate) fn sorted(
    context: &mut Context,
    items: Vec<Value>,
    closure: Option<&Rc<Closure>>,
) -> Result<Vec<Value>, Error> {
    let Some(closure) = closure else {
        let mut items = items;
        items.sort_by(|a, b| a.number_aware_compare(b));
        return Ok(items);
    };
    if closure.code.parameters.len() == 2 {
        // Insertion sort, which calls the comparator as few times as the order needs.
        let mut result: Vec<Value> = Vec::new();
        for item in items {
            let mut position = result.len();
            while position > 0 {
                let order = context
                    .interpreter
                    .call_closure(closure, vec![result[position - 1].clone(), item.clone()])?;
                let order = match &order {
                    Value::Number(number)
                        if !matches!(number, Number::Double(value) if !value.is_finite())
                            && !matches!(number, Number::Float(value) if !value.is_finite()) =>
                    {
                        number.to_i32()
                    }
                    _ => {
                        return Err(thrown_text(format!(
                            "Sort comparator must return a finite number, received: {order}"
                        )));
                    }
                };
                if order <= 0 {
                    break;
                }
                position -= 1;
            }
            result.insert(position, item);
        }
        return Ok(result);
    }
    sorted_keys(context, items, closure)
}

pub(crate) fn sorted_keys(
    context: &mut Context,
    items: Vec<Value>,
    closure: &Rc<Closure>,
) -> Result<Vec<Value>, Error> {
    let mut keyed = Vec::new();
    for item in items {
        let key = context
            .interpreter
            .call_closure(closure, vec![item.clone()])?;
        keyed.push((key, item));
    }
    keyed.sort_by(|a, b| a.0.number_aware_compare(&b.0));
    Ok(keyed.into_iter().map(|(_, item)| item).collect())
}

pub(crate) enum Sort {
    None,
    Natural,
    Key(Rc<Closure>),
    Compare(Rc<Closure>),
    Hash(bool),
}

impl Sort {
    pub fn new(value: Option<Value>, operator: &str) -> Result<Self, Error> {
        let collecting = operator == "collectFile";
        match value {
            None | Some(Value::Null) if !collecting => Ok(Self::None),
            Some(Value::Boolean(false)) => Ok(Self::None),
            Some(Value::Boolean(true)) => Ok(Self::Natural),
            Some(Value::String(text) | Value::GString(text))
                if matches!(&*text, "true" | "natural") =>
            {
                Ok(Self::Natural)
            }
            Some(Value::String(text) | Value::GString(text))
                if collecting && matches!(&*text, "none" | "index") =>
            {
                Ok(Self::None)
            }
            Some(Value::Closure(closure)) => {
                let count = closure.code.parameters.len();
                let implicit = closure.code.implicit_parameter;
                if !collecting && count == 2 {
                    return Ok(Self::Compare(closure));
                }
                let key = implicit
                    || if collecting {
                        count >= 1
                            && closure
                                .code
                                .parameters
                                .iter()
                                .skip(1)
                                .all(|parameter| parameter.default.is_some())
                    } else {
                        count == 1
                    };
                if key {
                    Ok(Self::Key(closure))
                } else {
                    Err(thrown_text(format!(
                        "Invalid `{operator}` sort closure arity: {count}"
                    )))
                }
            }
            None | Some(Value::Null) => Ok(Self::Hash(false)),
            Some(Value::String(text) | Value::GString(text))
                if matches!(&*text, "hash" | "deep") =>
            {
                Ok(Self::Hash(&*text == "deep"))
            }
            Some(value) => Err(thrown_text(format!(
                "Invalid `{operator}` sort option: {value}"
            ))),
        }
    }

    pub fn apply(&self, context: &mut Context, items: Vec<Value>) -> Result<Vec<Value>, Error> {
        match self {
            Self::None => Ok(items),
            Self::Natural => sorted(context, items, None),
            Self::Key(closure) => sorted_keys(context, items, closure),
            Self::Compare(closure) => sorted(context, items, Some(closure)),
            Self::Hash(_) if items.len() < 2 => Ok(items),
            Self::Hash(deep) => {
                let mut keyed = Vec::with_capacity(items.len());
                for item in items {
                    keyed.push((
                        crate::hash::key(&item, context.interpreter.host(), *deep)?,
                        item,
                    ));
                }
                keyed.sort_by_key(|(key, _)| *key);
                Ok(keyed.into_iter().map(|(_, item)| item).collect())
            }
        }
    }
}

/// `collate(size, [step], remainder = true)`: windows of `size` values; a new window starts
/// every `step` values. At the end the open windows are emitted when `remainder` is true.
pub(crate) struct Collate {
    pub output: ChannelId,
    pub size: usize,
    pub step: usize,
    pub remainder: bool,
    pub windows: Vec<Vec<Value>>,
    pub count: usize,
}

impl Behaviour for Collate {
    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        while let Some(message) = context.read(0) {
            let Message::Value(value) = message else {
                if self.remainder {
                    for window in std::mem::take(&mut self.windows) {
                        if !window.is_empty() {
                            context.emit(self.output, Value::list(window));
                        }
                    }
                }
                context.close(self.output);
                return Ok(State::Stopped);
            };
            if self.count.is_multiple_of(self.step) {
                self.windows.push(Vec::new());
            }
            self.count += 1;
            for window in &mut self.windows {
                window.push(value.clone());
            }
            while self
                .windows
                .first()
                .is_some_and(|window| window.len() == self.size)
            {
                context.emit(self.output, Value::list(self.windows.remove(0)));
            }
        }
        Ok(State::Running)
    }
}

/// `buffer`: lists of values between an opening and a closing condition, as Nextflow's
/// `BufferOp` collects them. With `size` and `skip`, the buffer opens after `skip` values
/// and closes after `size` more; else criteria decide, with `isCase` or a closure.
pub(crate) struct Buffer {
    pub output: ChannelId,
    pub opening: Opening,
    pub closing: Closing,
    /// Whether the values of a buffer that never closed are emitted at the end.
    pub remainder: bool,
    pub buffer: Vec<Value>,
    pub open: bool,
}

pub(crate) enum Opening {
    /// Always open.
    Always,
    /// Opens after `skip` values, counted from the last close.
    Skip {
        skip: usize,
        count: usize,
    },
    Criteria(Value),
}

pub(crate) enum Closing {
    /// Closes when `size` values after the skipped ones have come.
    Size {
        size: usize,
        skip: usize,
        count: usize,
    },
    Criteria(Value),
}

impl Buffer {
    fn opens(&mut self, context: &mut Context, value: &Value) -> Result<bool, Error> {
        match &mut self.opening {
            Opening::Always => Ok(true),
            Opening::Skip { skip, count } => {
                *count += 1;
                if *count > *skip {
                    *count = 0;
                    return Ok(true);
                }
                Ok(false)
            }
            Opening::Criteria(criteria) => matches(context, criteria, value),
        }
    }

    fn closes(&mut self, context: &mut Context, value: &Value) -> Result<bool, Error> {
        match &mut self.closing {
            Closing::Size { size, skip, count } => {
                *count += 1;
                if *count == *size + *skip {
                    *count = 0;
                    return Ok(true);
                }
                Ok(false)
            }
            Closing::Criteria(criteria) => matches(context, criteria, value),
        }
    }
}

/// Whether a value meets a condition: the truth of a closure's result, or `isCase`.
fn matches(context: &mut Context, criteria: &Value, value: &Value) -> Result<bool, Error> {
    Ok(match criteria {
        Value::Closure(closure) => context
            .interpreter
            .call_closure(closure, vec![value.clone()])?
            .truth(),
        other => other.is_case(value).map_err(thrown)?,
    })
}

impl Behaviour for Buffer {
    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
        while let Some(message) = context.read(0) {
            let Message::Value(value) = message else {
                if self.remainder && !self.buffer.is_empty() {
                    context.emit(self.output, Value::list(std::mem::take(&mut self.buffer)));
                }
                context.close(self.output);
                return Ok(State::Stopped);
            };
            if self.open {
                self.buffer.push(value.clone());
            } else if self.opens(context, &value)? {
                self.open = true;
                self.buffer.push(value.clone());
            }
            if self.closes(context, &value)? {
                context.emit(self.output, Value::list(std::mem::take(&mut self.buffer)));
                self.open = matches!(self.opening, Opening::Always);
            }
        }
        Ok(State::Running)
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

/// The leaves of nested lists and sets, including nulls; maps stay whole.
fn flatten(value: Value, leaves: &mut Vec<Value>) {
    match value {
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

fn thrown_text(message: String) -> Error {
    thrown(nextflow_evaluator::exception::Exception::illegal_argument(
        message,
    ))
}

fn thrown(exception: nextflow_evaluator::exception::Exception) -> Error {
    Error::Thrown {
        exception: Rc::new(exception),
        span: Default::default(),
    }
}

/// A method that is no operator, called on a value channel: it is called on the value once
/// the channel and the value channels among the arguments are bound, and its result binds a
/// new value channel, as GPars' `DataflowInvocationExpression` does.
pub(crate) struct Invoke {
    pub output: ChannelId,
    pub name: Rc<str>,
    /// The arguments; `None` stands for the next input after the receiver.
    pub arguments: Vec<Option<Value>>,
    /// Whether `name` is a property to read rather than a method to call.
    pub property: bool,
}

impl Behaviour for Invoke {
    fn run(&mut self, context: &mut Context) -> Result<State, Error> {
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
        let mut values = values.into_iter();
        let object = values.next().unwrap_or(Value::Null);
        let arguments = self
            .arguments
            .iter()
            .map(|argument| match argument {
                Some(argument) => argument.clone(),
                None => values.next().unwrap_or(Value::Null),
            })
            .collect();
        let result = if self.property {
            context.interpreter.read_property(&object, &self.name)?
        } else {
            context
                .interpreter
                .invoke_method(&object, &self.name, arguments)?
        };
        context.emit(self.output, result);
        context.close(self.output);
        Ok(State::Stopped)
    }
}
