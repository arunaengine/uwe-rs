//! Methods of maps and map entries. A closure with two parameters gets the key and the value,
//! any other closure gets the `Map.Entry` (Groovy spec, section 9.3).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use indexmap::IndexMap;
use nextflow_parser::Span;

use super::super::{AtSpan, Error, Interpreter, thrown};
use super::iterable::elements;
use crate::exception::Exception;
use crate::value::{Closure, Key, Map, MapKind, Operator, Value};

impl Interpreter<'_> {
    pub(super) fn map_method(
        &mut self,
        map: &Rc<RefCell<Map>>,
        name: &str,
        arguments: &[Value],
        span: Span,
    ) -> Result<Option<Value>, Error> {
        let object = Value::Map(map.clone());
        let entries: Vec<(Value, Value)> = map
            .borrow()
            .entries
            .iter()
            .map(|(key, value)| (key.0.clone(), value.clone()))
            .collect();
        let kind = map.borrow().kind;
        let changing = matches!(name, "put" | "putAll" | "remove" | "clear")
            || (name == "get" && arguments.len() == 2);
        if changing && kind == MapKind::Record {
            return Err(thrown(
                Exception::new("UnsupportedOperationException", ""),
                span,
            ));
        }
        let get = |key: &Value| map.borrow().entries.get(&Key(key.clone())).cloned();
        Ok(Some(match (name, arguments) {
            ("size", []) => Value::integer(entries.len() as i32),
            ("isEmpty", []) => Value::Boolean(entries.is_empty()),
            ("containsKey", [key]) => Value::Boolean(get(key).is_some()),
            ("containsValue", [value]) => {
                Value::Boolean(entries.iter().any(|(_, other)| other.java_equals(value)))
            }
            ("get" | "getAt", [key]) => match get(key) {
                Some(value) => value,
                None => self.default_value(map, key, span)?,
            },
            ("withDefault", [Value::Closure(f)]) => {
                let copy = super::copy_map(&object);
                if let Value::Map(copy) = &copy {
                    copy.borrow_mut().default = Some(f.clone());
                }
                copy
            }
            ("getOrDefault", [key, default]) => get(key).unwrap_or_else(|| default.clone()),
            ("get", [key, default]) => match get(key) {
                Some(value) => value,
                None => {
                    map.borrow_mut()
                        .entries
                        .insert(Key(key.clone()), default.clone());
                    default.clone()
                }
            },
            ("put", [key, value]) => {
                let old = map
                    .borrow_mut()
                    .entries
                    .insert(Key(key.clone()), value.clone());
                old.unwrap_or(Value::Null)
            }
            ("putAll", [Value::Map(other)]) => {
                let other = other.borrow().entries.clone();
                map.borrow_mut().entries.extend(other);
                Value::Null
            }
            ("remove", [key]) => {
                let old = map.borrow_mut().entries.shift_remove(&Key(key.clone()));
                old.unwrap_or(Value::Null)
            }
            ("clear", []) => {
                map.borrow_mut().entries.clear();
                Value::Null
            }
            ("keySet", []) => Value::set(entries.into_iter().map(|(key, _)| key)),
            ("values", []) => Value::list(entries.into_iter().map(|(_, value)| value).collect()),
            ("entrySet", []) => Value::set(entries.into_iter().map(entry)),
            ("each", [Value::Closure(f)]) => {
                for (key, value) in &entries {
                    self.call_closure_value(f, pair(f, key, value), span)?;
                }
                object
            }
            ("eachWithIndex", [Value::Closure(f)]) => {
                for (index, (key, value)) in entries.iter().enumerate() {
                    let index = Value::integer(index as i32);
                    let arguments = if parameter_count(f) == 3 {
                        vec![key.clone(), value.clone(), index]
                    } else {
                        vec![entry((key.clone(), value.clone())), index]
                    };
                    self.call_closure_value(f, arguments, span)?;
                }
                object
            }
            ("collect", [Value::Closure(f)]) => Value::list(self.map_entries(&entries, f, span)?),
            ("findAll" | "find" | "any" | "every" | "count", [Value::Closure(f)]) => {
                let passed: Vec<bool> = self
                    .map_entries(&entries, f, span)?
                    .iter()
                    .map(Value::truth)
                    .collect();
                let mut chosen = entries
                    .iter()
                    .zip(&passed)
                    .filter(|(_, p)| **p)
                    .map(|(e, _)| e);
                match name {
                    "findAll" => Value::map(chosen.cloned()),
                    "find" => chosen.next().cloned().map_or(Value::Null, entry),
                    "any" => Value::Boolean(passed.contains(&true)),
                    "every" => Value::Boolean(passed.iter().all(|passed| *passed)),
                    _ => Value::integer(passed.iter().filter(|passed| **passed).count() as i32),
                }
            }
            ("collectEntries", [Value::Closure(f)]) => {
                let results = self.map_entries(&entries, f, span)?;
                let mut collected = IndexMap::new();
                for result in results {
                    match result {
                        Value::Map(other) => collected.extend(other.borrow().entries.clone()),
                        Value::MapEntry(pair) => {
                            collected.insert(Key(pair.0.clone()), pair.1.clone());
                        }
                        other => {
                            let pair = elements(&other);
                            let item =
                                |index: usize| pair.get(index).cloned().unwrap_or(Value::Null);
                            collected.insert(Key(item(0)), item(1));
                        }
                    }
                }
                Value::map(collected.into_iter().map(|(key, value)| (key.0, value)))
            }
            // `workflow.manifest` is a map here; its `toMap()` gives a copy.
            ("toMap", []) => super::copy_map(&Value::Map(map.clone())),
            // An unmodifiable view; changes through it are not checked yet.
            ("asImmutable" | "asUnmodifiable", []) => Value::Map(map.clone()),
            ("groupBy", [Value::Closure(f)]) => {
                let keys = self.map_entries(&entries, f, span)?;
                let mut groups: IndexMap<Key, Vec<(Value, Value)>> = IndexMap::new();
                for (key, pair) in keys.into_iter().zip(entries) {
                    groups.entry(Key(key)).or_default().push(pair);
                }
                Value::map(
                    groups
                        .into_iter()
                        .map(|(key, group)| (key.0, Value::map(group))),
                )
            }
            ("inject", [initial, Value::Closure(f)]) => {
                let mut accumulator = initial.clone();
                for (key, value) in &entries {
                    let arguments = if parameter_count(f) == 3 {
                        vec![accumulator, key.clone(), value.clone()]
                    } else {
                        vec![accumulator, entry((key.clone(), value.clone()))]
                    };
                    accumulator = self.call_closure_value(f, arguments, span)?;
                }
                accumulator
            }
            ("subMap", keys) if !keys.is_empty() => {
                let keys = match keys {
                    [single] => elements(single),
                    several => several.to_vec(),
                };
                let chosen = keys
                    .into_iter()
                    .filter_map(|key| get(&key).map(|value| (key, value)))
                    .collect::<Vec<_>>();
                let sub = Value::map(chosen);
                if let (Value::Map(sub), MapKind::Record) = (&sub, kind) {
                    sub.borrow_mut().kind = MapKind::Record;
                }
                sub
            }
            ("toSorted", []) => {
                let mut sorted = entries;
                sorted.sort_by(|a, b| a.0.number_aware_compare(&b.0));
                Value::map(sorted)
            }
            ("toSorted", [Value::Closure(f)]) => {
                let mut sorted: Vec<Value> = entries.into_iter().map(entry).collect();
                self.sort_with(&mut sorted, f, span)?;
                Value::map(sorted.into_iter().filter_map(|item| match item {
                    Value::MapEntry(pair) => Some((pair.0.clone(), pair.1.clone())),
                    _ => None,
                }))
            }
            ("sort", []) => {
                let mut sorted = entries;
                sorted.sort_by(|a, b| a.0.number_aware_compare(&b.0));
                let sorted = Value::map(sorted);
                if let Value::Map(inner) = &sorted {
                    inner.borrow_mut().kind = MapKind::TreeMap;
                }
                sorted
            }
            ("clone", []) => {
                let copy = Value::map(entries);
                if let Value::Map(inner) = &copy {
                    inner.borrow_mut().kind = kind;
                }
                copy
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

    /// The closure's result for each entry: `(key, value)` or the entry.
    fn map_entries(
        &mut self,
        entries: &[(Value, Value)],
        f: &Rc<Closure>,
        span: Span,
    ) -> Result<Vec<Value>, Error> {
        entries
            .iter()
            .map(|(key, value)| self.call_closure_value(f, pair(f, key, value), span))
            .collect()
    }
}

/// Methods of a `Map.Entry`.
pub(super) fn entry_method(
    pair: &(Value, Value),
    name: &str,
    arguments: &[Value],
) -> Option<Value> {
    match (name, arguments) {
        ("getKey", []) => Some(pair.0.clone()),
        ("getValue", []) => Some(pair.1.clone()),
        _ => None,
    }
}

fn entry(pair: (Value, Value)) -> Value {
    Value::MapEntry(Rc::new(pair))
}

/// The arguments for a closure over one entry: key and value for two parameters, else the entry.
fn pair(f: &Closure, key: &Value, value: &Value) -> Vec<Value> {
    if parameter_count(f) == 2 {
        vec![key.clone(), value.clone()]
    } else {
        vec![entry((key.clone(), value.clone()))]
    }
}

/// Groovy's `getMaximumNumberOfParameters`: one for a closure without a parameter list.
fn parameter_count(f: &Closure) -> usize {
    if f.code.implicit_parameter {
        1
    } else {
        f.code.parameters.len()
    }
}
