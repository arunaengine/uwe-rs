//! Methods that lists and sets share: Groovy's `DefaultGroovyMethods` on `Iterable`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cmp::Ordering;
use std::rc::Rc;

use nextflow_parser::Span;

use super::super::{AtSpan, Error, Interpreter, thrown};
use crate::exception::Exception;
use crate::value::{Closure, Key, Operator, Set, SetKind, Value};

impl Interpreter<'_> {
    pub(super) fn iterable_method(
        &mut self,
        object: &Value,
        items: &[Value],
        name: &str,
        arguments: &[Value],
        span: Span,
    ) -> Result<Option<Value>, Error> {
        Ok(Some(match (name, arguments) {
            ("size", []) => Value::integer(items.len() as i32),
            ("isEmpty", []) => Value::Boolean(items.is_empty()),
            ("contains", [item]) => {
                Value::Boolean(items.iter().any(|other| other.java_equals(item)))
            }
            ("containsAll", [others]) => Value::Boolean(
                elements(others)
                    .iter()
                    .all(|item| items.iter().any(|e| e.java_equals(item))),
            ),
            ("each", [Value::Closure(f)]) => {
                for item in items {
                    self.call_closure_value(f, vec![item.clone()], span)?;
                }
                object.clone()
            }
            ("eachWithIndex", [Value::Closure(f)]) => {
                for (index, item) in items.iter().enumerate() {
                    let arguments = vec![item.clone(), Value::integer(index as i32)];
                    self.call_closure_value(f, arguments, span)?;
                }
                object.clone()
            }
            ("collect", []) => Value::list(items.to_vec()),
            ("collect", [Value::Closure(f)]) => Value::list(self.map_items(items, f, span)?),
            ("collectMany", [Value::Closure(f)]) => {
                let results = self.map_items(items, f, span)?;
                Value::list(results.iter().flat_map(elements).collect())
            }
            ("findAll", []) => {
                Value::list(items.iter().filter(|item| item.truth()).cloned().collect())
            }
            (
                "findAll" | "find" | "any" | "every" | "count" | "findIndexOf",
                [Value::Closure(f)],
            ) => {
                let mut passed = Vec::new();
                for item in items {
                    passed.push(
                        self.call_closure_value(f, vec![item.clone()], span)?
                            .truth(),
                    );
                }
                let first = passed.iter().position(|passed| *passed);
                match name {
                    "findAll" => Value::list(
                        items
                            .iter()
                            .zip(&passed)
                            .filter(|(_, p)| **p)
                            .map(|(i, _)| i.clone())
                            .collect(),
                    ),
                    "find" => first.map_or(Value::Null, |index| items[index].clone()),
                    "any" => Value::Boolean(first.is_some()),
                    "every" => Value::Boolean(passed.iter().all(|passed| *passed)),
                    "count" => {
                        Value::integer(passed.iter().filter(|passed| **passed).count() as i32)
                    }
                    _ => Value::integer(first.map_or(-1, |index| index as i32)),
                }
            }
            ("any", []) => Value::Boolean(items.iter().any(Value::truth)),
            ("every", []) => Value::Boolean(items.iter().all(Value::truth)),
            ("count", [item]) => Value::integer(
                items
                    .iter()
                    .filter(|other| other.coerced_equals(item))
                    .count() as i32,
            ),
            ("findResults", [Value::Closure(f)]) => {
                let results = self.map_items(items, f, span)?;
                Value::list(
                    results
                        .into_iter()
                        .filter(|value| !matches!(value, Value::Null))
                        .collect(),
                )
            }
            ("join", []) => Value::string(join(items, "")),
            ("join", [separator]) => Value::string(join(items, &separator.to_string())),
            ("sum", []) => sum(items.iter().cloned(), span)?,
            ("sum", [Value::Closure(f)]) => {
                let results = self.map_items(items, f, span)?;
                sum(results.into_iter(), span)?
            }
            ("inject", [Value::Closure(f)]) => {
                let Some((first, rest)) = items.split_first() else {
                    let message = "Cannot call inject() on an empty collection without passing an initial value.";
                    let exception = Exception::new("NoSuchElementException", message);
                    return Err(thrown(exception, span));
                };
                self.fold(first.clone(), rest, f, span)?
            }
            ("inject", [initial, Value::Closure(f)]) => {
                self.fold(initial.clone(), items, f, span)?
            }
            ("max" | "min", []) => extreme(items, name == "max", span)?,
            ("max" | "min", [Value::Closure(f)]) => {
                let keys = self.map_items(items, f, span)?;
                let mut best: Option<usize> = None;
                for index in 0..items.len() {
                    let better = match best {
                        None => true,
                        Some(current) => {
                            let order = keys[index].groovy_compare(&keys[current]).at(span)?;
                            if name == "max" {
                                order.is_gt()
                            } else {
                                order.is_lt()
                            }
                        }
                    };
                    if better {
                        best = Some(index);
                    }
                }
                best.map_or(Value::Null, |index| items[index].clone())
            }
            ("intersect", [others]) => {
                let others = elements(others);
                let kept = items.iter().filter(|item| {
                    others
                        .iter()
                        .any(|other| item.number_aware_compare(other) == Ordering::Equal)
                });
                Value::list(kept.cloned().collect())
            }
            ("groupBy", [Value::Closure(f)]) => {
                let keys = self.map_items(items, f, span)?;
                let mut groups: indexmap::IndexMap<Key, Vec<Value>> = indexmap::IndexMap::new();
                for (key, item) in keys.into_iter().zip(items) {
                    groups.entry(Key(key)).or_default().push(item.clone());
                }
                Value::map(
                    groups
                        .into_iter()
                        .map(|(key, group)| (key.0, Value::list(group))),
                )
            }
            ("countBy", [Value::Closure(f)]) => {
                let keys = self.map_items(items, f, span)?;
                let mut counts: indexmap::IndexMap<Key, i32> = indexmap::IndexMap::new();
                for key in keys {
                    *counts.entry(Key(key)).or_default() += 1;
                }
                Value::map(
                    counts
                        .into_iter()
                        .map(|(key, count)| (key.0, Value::integer(count))),
                )
            }
            ("collectEntries", []) => Value::map(entries(items.to_vec())),
            ("collectEntries", [Value::Closure(f)]) => {
                let results = self.map_items(items, f, span)?;
                Value::map(entries(results))
            }
            ("flatten", []) => Value::list(flatten(items)),
            ("withIndex", rest @ ([] | [Value::Number(_)])) => {
                let offset = match rest {
                    [Value::Number(offset)] => offset.to_i64() as i32,
                    _ => 0,
                };
                Value::list(
                    items
                        .iter()
                        .enumerate()
                        .map(|(index, item)| {
                            Value::list(vec![item.clone(), Value::integer(index as i32 + offset)])
                        })
                        .collect(),
                )
            }
            ("toList", []) => Value::list(items.to_vec()),
            // Groovy's `toSet()` is `new HashSet(size())`.
            ("toSet", []) => hash_set(items.len().next_power_of_two(), items),
            ("grep", []) => {
                Value::list(items.iter().filter(|item| item.truth()).cloned().collect())
            }
            // `grep(filter)` keeps the elements the filter accepts, as `isCase` decides.
            ("grep", [filter]) => {
                let mut chosen = Vec::new();
                for item in items {
                    let accepted = match filter {
                        Value::Closure(f) => self
                            .call_closure_value(f, vec![item.clone()], span)?
                            .truth(),
                        other => other.is_case(item).at(span)?,
                    };
                    if accepted {
                        chosen.push(item.clone());
                    }
                }
                Value::list(chosen)
            }
            // Other iterables, like a path of name parts, sort into a new list.
            ("sort", rest) => {
                let mut sorted = items.to_vec();
                match rest {
                    [] | [Value::Boolean(_)] => sorted.sort_by(Value::number_aware_compare),
                    [Value::Closure(f)] | [Value::Boolean(_), Value::Closure(f)] => {
                        self.sort_with(&mut sorted, f, span)?
                    }
                    _ => return Ok(None),
                }
                Value::list(sorted)
            }
            ("plus" | "minus", [other]) => {
                let operator = if name == "plus" {
                    Operator::Add
                } else {
                    Operator::Subtract
                };
                object.apply(operator, other).at(span)?
            }
            _ => return Ok(None),
        }))
    }

    /// The closure's result for each element.
    pub(super) fn map_items(
        &mut self,
        items: &[Value],
        f: &Rc<Closure>,
        span: Span,
    ) -> Result<Vec<Value>, Error> {
        items
            .iter()
            .map(|item| self.call_closure_value(f, vec![item.clone()], span))
            .collect()
    }

    fn fold(
        &mut self,
        initial: Value,
        items: &[Value],
        f: &Rc<Closure>,
        span: Span,
    ) -> Result<Value, Error> {
        let mut accumulator = initial;
        for item in items {
            accumulator = self.call_closure_value(f, vec![accumulator, item.clone()], span)?;
        }
        Ok(accumulator)
    }
}

/// The elements of a list or set, or the value alone.
pub(in crate::interpreter) fn elements(value: &Value) -> Vec<Value> {
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

fn join(items: &[Value], separator: &str) -> String {
    items
        .iter()
        .map(Value::to_groovy_string)
        .collect::<Vec<_>>()
        .join(separator)
}

/// `sum()`: the elements added with Groovy's `+`; `null` for no elements.
fn sum(mut items: impl Iterator<Item = Value>, span: Span) -> Result<Value, Error> {
    let Some(mut total) = items.next() else {
        return Ok(Value::Null);
    };
    for item in items {
        total = total.apply(Operator::Add, &item).at(span)?;
    }
    Ok(total)
}

/// `max()` or `min()` by Groovy's `>` or `<`, skipping `null` elements.
fn extreme(items: &[Value], max: bool, span: Span) -> Result<Value, Error> {
    let mut best: Option<&Value> = None;
    for item in items.iter().filter(|item| !matches!(item, Value::Null)) {
        let better = match best {
            None => true,
            Some(current) => {
                let order = item.groovy_compare(current).at(span)?;
                if max { order.is_gt() } else { order.is_lt() }
            }
        };
        if better {
            best = Some(item);
        }
    }
    Ok(best.cloned().unwrap_or(Value::Null))
}

/// Map entries from values that are maps, entries or two-element lists.
fn entries(values: Vec<Value>) -> Vec<(Value, Value)> {
    let mut entries = Vec::new();
    for value in values {
        match value {
            Value::Map(map) => {
                entries.extend(
                    map.borrow()
                        .entries
                        .iter()
                        .map(|(k, v)| (k.0.clone(), v.clone())),
                );
            }
            Value::MapEntry(entry) => entries.push((entry.0.clone(), entry.1.clone())),
            Value::List(list) => {
                let list = list.borrow();
                let item = |index: usize| list.items.get(index).cloned().unwrap_or(Value::Null);
                entries.push((item(0), item(1)));
            }
            _ => {}
        }
    }
    entries
}

fn flatten(items: &[Value]) -> Vec<Value> {
    let mut flat = Vec::new();
    for item in items {
        match item {
            Value::List(_) | Value::Set(_) => flat.extend(flatten(&elements(item))),
            other => flat.push(other.clone()),
        }
    }
    flat
}

/// A `HashSet` with a table of `capacity`, which must be a power of two, and the items.
pub(in crate::interpreter) fn hash_set(capacity: usize, items: &[Value]) -> Value {
    let mut set = Set {
        kind: SetKind::HashSet(capacity.max(1)),
        items: Default::default(),
    };
    for item in items {
        set.add(item.clone());
    }
    Value::Set(Rc::new(std::cell::RefCell::new(set)))
}
