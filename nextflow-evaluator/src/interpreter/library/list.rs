//! Methods of lists and sets that read by position, sort, de-duplicate or change the receiver.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::cmp::Ordering;
use std::rc::Rc;

use nextflow_parser::Span;

use super::super::{Error, Interpreter, thrown};
use super::iterable::elements;
use crate::exception::Exception;
use crate::number::Number;
use crate::value::{Closure, Key, List, ListKind, Set, Value};

impl Interpreter<'_> {
    pub(super) fn list_method(
        &mut self,
        list: &Rc<RefCell<List>>,
        name: &str,
        arguments: &[Value],
        span: Span,
    ) -> Result<Option<Value>, Error> {
        let items = list.borrow().items.clone();
        let kind = list.borrow().kind;
        let fixed = matches!(kind, ListKind::Tuple | ListKind::Array | ListKind::Range);
        let empty = |what: &str| {
            let message = format!("Cannot access {what}() element from an empty List");
            Err(thrown(
                Exception::new("NoSuchElementException", message),
                span,
            ))
        };
        Ok(Some(match (name, arguments) {
            ("get", [Value::Number(index)]) => match usize::try_from(index.to_i64()) {
                Ok(position) if position < items.len() => items[position].clone(),
                _ => {
                    let message = format!("Index {index} out of bounds for length {}", items.len());
                    return Err(thrown(
                        Exception::new("IndexOutOfBoundsException", message),
                        span,
                    ));
                }
            },
            ("getAt", [index]) => self.index(
                &Value::List(list.clone()),
                std::slice::from_ref(index),
                span,
            )?,
            ("first" | "head", []) => match items.first() {
                Some(item) => item.clone(),
                None => return empty("first"),
            },
            ("last", []) => match items.last() {
                Some(item) => item.clone(),
                None => return empty("last"),
            },
            ("tail", []) if !items.is_empty() => Value::list(items[1..].to_vec()),
            ("init", []) if !items.is_empty() => Value::list(items[..items.len() - 1].to_vec()),
            ("take" | "drop", [Value::Number(count)]) => {
                let count = (count.to_i64().max(0) as usize).min(items.len());
                let part = if name == "take" {
                    &items[..count]
                } else {
                    &items[count..]
                };
                Value::list(part.to_vec())
            }
            ("takeWhile" | "dropWhile", [Value::Closure(f)]) => {
                let mut count = 0;
                for item in &items {
                    if !self
                        .call_closure_value(f, vec![item.clone()], span)?
                        .truth()
                    {
                        break;
                    }
                    count += 1;
                }
                let part = if name == "takeWhile" {
                    &items[..count]
                } else {
                    &items[count..]
                };
                Value::list(part.to_vec())
            }
            ("reverse", []) => Value::list(items.iter().rev().cloned().collect()),
            ("subList", [Value::Number(from), Value::Number(to)]) => {
                let (from, to) = (from.to_i64(), to.to_i64());
                if from < 0 || to > items.len() as i64 || from > to {
                    let message =
                        format!("fromIndex: {from}, toIndex: {to}, size: {}", items.len());
                    return Err(thrown(
                        Exception::new("IndexOutOfBoundsException", message),
                        span,
                    ));
                }
                Value::list(items[from as usize..to as usize].to_vec())
            }
            ("indexOf" | "lastIndexOf", [item]) => {
                let mut positions = items
                    .iter()
                    .enumerate()
                    .filter(|(_, other)| other.java_equals(item));
                let found = if name == "indexOf" {
                    positions.next()
                } else {
                    positions.next_back()
                };
                Value::integer(found.map_or(-1, |(index, _)| index as i32))
            }
            ("collate", [Value::Number(size), rest @ ..]) => {
                let size = size.to_i64().max(1) as usize;
                let (step, keep) = match rest {
                    [] => (size, true),
                    [Value::Boolean(keep)] => (size, *keep),
                    [Value::Number(step)] => (step.to_i64().max(1) as usize, true),
                    [Value::Number(step), Value::Boolean(keep)] => {
                        (step.to_i64().max(1) as usize, *keep)
                    }
                    _ => return Ok(None),
                };
                let windows = (0..items.len())
                    .step_by(step)
                    .map(|start| items[start..(start + size).min(items.len())].to_vec())
                    .filter(|window| keep || window.len() == size)
                    .map(Value::list)
                    .collect();
                Value::list(windows)
            }
            ("transpose", []) => {
                let rows: Vec<Vec<Value>> = items.iter().map(elements).collect();
                let width = rows.iter().map(Vec::len).min().unwrap_or(0);
                let columns = (0..width)
                    .map(|column| Value::list(rows.iter().map(|row| row[column].clone()).collect()))
                    .collect();
                Value::list(columns)
            }
            ("sort" | "toSorted", rest) => {
                let mut sorted = items.clone();
                match rest {
                    [] => sorted.sort_by(Value::number_aware_compare),
                    [Value::Closure(f)] => self.sort_with(&mut sorted, f, span)?,
                    _ => return Ok(None),
                }
                if name == "sort" {
                    list.borrow_mut().items = sorted;
                    Value::List(list.clone())
                } else {
                    Value::list(sorted)
                }
            }
            ("unique" | "toUnique", rest) => {
                let unique = match rest {
                    [] => unique(&items, |a, b| Ok(a.coerced_equals(b)))?,
                    [Value::Closure(f)] if f.code.parameters.len() == 2 => {
                        let f = f.clone();
                        unique(&items, |a, b| {
                            let order =
                                self.call_closure_value(&f, vec![a.clone(), b.clone()], span)?;
                            Ok(matches!(order, Value::Number(n) if n.is_zero()))
                        })?
                    }
                    [Value::Closure(f)] => {
                        let keys = self.map_items(&items, f, span)?;
                        let positions: Vec<usize> = (0..items.len()).collect();
                        let kept = unique_by(&positions, |a, b| keys[*a].coerced_equals(&keys[*b]));
                        kept.into_iter().map(|index| items[index].clone()).collect()
                    }
                    _ => return Ok(None),
                };
                if name == "unique" {
                    if kind == ListKind::Tuple {
                        return Err(rejected(span));
                    }
                    list.borrow_mut().items = unique;
                    Value::List(list.clone())
                } else {
                    Value::list(unique)
                }
            }
            ("add" | "addAll" | "remove" | "removeAll" | "clear" | "set" | "leftShift", _)
                if fixed =>
            {
                if name == "set" && kind == ListKind::Array {
                    return self.set_list_item(list, arguments, span);
                }
                return Err(rejected(span));
            }
            ("add", [item]) => {
                list.borrow_mut().items.push(item.clone());
                Value::Boolean(true)
            }
            ("add", [Value::Number(index), item]) => {
                let position = usize::try_from(index.to_i64())
                    .ok()
                    .filter(|p| *p <= items.len());
                let Some(position) = position else {
                    let message = format!("Index: {index}, Size: {}", items.len());
                    return Err(thrown(
                        Exception::new("IndexOutOfBoundsException", message),
                        span,
                    ));
                };
                list.borrow_mut().items.insert(position, item.clone());
                Value::Null
            }
            ("addAll", [others]) => {
                let others = elements(others);
                let changed = !others.is_empty();
                list.borrow_mut().items.extend(others);
                Value::Boolean(changed)
            }
            ("remove", [Value::Number(index @ Number::Integer(_))]) => {
                match usize::try_from(index.to_i64())
                    .ok()
                    .filter(|p| *p < items.len())
                {
                    Some(position) => list.borrow_mut().items.remove(position),
                    None => {
                        let message =
                            format!("Index {index} out of bounds for length {}", items.len());
                        return Err(thrown(
                            Exception::new("IndexOutOfBoundsException", message),
                            span,
                        ));
                    }
                }
            }
            ("remove", [item]) => {
                let position = items.iter().position(|other| other.java_equals(item));
                if let Some(position) = position {
                    list.borrow_mut().items.remove(position);
                }
                Value::Boolean(position.is_some())
            }
            ("removeAll", [others]) => {
                let others = elements(others);
                list.borrow_mut()
                    .items
                    .retain(|item| !others.iter().any(|o| o.java_equals(item)));
                Value::Boolean(list.borrow().items.len() != items.len())
            }
            ("removeIf", [Value::Closure(f)]) => {
                let mut kept = Vec::new();
                for item in &items {
                    if !self
                        .call_closure_value(f, vec![item.clone()], span)?
                        .truth()
                    {
                        kept.push(item.clone());
                    }
                }
                let changed = kept.len() != items.len();
                list.borrow_mut().items = kept;
                Value::Boolean(changed)
            }
            ("clear", []) => {
                list.borrow_mut().items.clear();
                Value::Null
            }
            ("set", [_, _]) => return self.set_list_item(list, arguments, span),
            _ => return Ok(None),
        }))
    }

    pub(super) fn set_method(
        &mut self,
        set: &Rc<RefCell<Set>>,
        name: &str,
        arguments: &[Value],
        span: Span,
    ) -> Result<Option<Value>, Error> {
        let items: Vec<Value> = set
            .borrow()
            .items
            .iter()
            .map(|item| item.0.clone())
            .collect();
        Ok(Some(match (name, arguments) {
            ("add", [item]) => Value::Boolean(set.borrow_mut().items.insert(Key(item.clone()))),
            ("addAll", [others]) => {
                let before = items.len();
                set.borrow_mut()
                    .items
                    .extend(elements(others).into_iter().map(Key));
                Value::Boolean(set.borrow().items.len() != before)
            }
            ("remove", [item]) => {
                Value::Boolean(set.borrow_mut().items.shift_remove(&Key(item.clone())))
            }
            ("clear", []) => {
                set.borrow_mut().items.clear();
                Value::Null
            }
            ("sort" | "toSorted", rest) => {
                let mut sorted = items;
                match rest {
                    [] => sorted.sort_by(Value::number_aware_compare),
                    [Value::Closure(f)] => self.sort_with(&mut sorted, f, span)?,
                    _ => return Ok(None),
                }
                Value::list(sorted)
            }
            _ => return Ok(None),
        }))
    }

    /// Sorts stably with a closure: one parameter gives a key, two parameters compare.
    fn sort_with(&mut self, items: &mut [Value], f: &Rc<Closure>, span: Span) -> Result<(), Error> {
        let mut failure = None;
        if f.code.parameters.len() == 2 {
            items.sort_by(|a, b| {
                match self.call_closure_value(f, vec![a.clone(), b.clone()], span) {
                    Ok(Value::Number(order)) => order.to_i32().cmp(&0),
                    Ok(_) => Ordering::Equal,
                    Err(error) => {
                        failure.get_or_insert(error);
                        Ordering::Equal
                    }
                }
            });
        } else {
            let keys = self.map_items(items, f, span)?;
            let mut indexed: Vec<(Value, Value)> =
                keys.into_iter().zip(items.iter().cloned()).collect();
            indexed.sort_by(|a, b| a.0.number_aware_compare(&b.0));
            for (slot, (_, item)) in items.iter_mut().zip(indexed) {
                *slot = item;
            }
        }
        failure.map_or(Ok(()), Err)
    }

    fn set_list_item(
        &mut self,
        list: &Rc<RefCell<List>>,
        arguments: &[Value],
        span: Span,
    ) -> Result<Option<Value>, Error> {
        let [Value::Number(index), item] = arguments else {
            return Ok(None);
        };
        let length = list.borrow().items.len();
        let Some(position) = usize::try_from(index.to_i64()).ok().filter(|p| *p < length) else {
            let message = format!("Index {index} out of bounds for length {length}");
            return Err(thrown(
                Exception::new("IndexOutOfBoundsException", message),
                span,
            ));
        };
        let old = std::mem::replace(&mut list.borrow_mut().items[position], item.clone());
        Ok(Some(old))
    }
}

/// The elements without later duplicates, where `same` decides duplicates.
fn unique(
    items: &[Value],
    mut same: impl FnMut(&Value, &Value) -> Result<bool, Error>,
) -> Result<Vec<Value>, Error> {
    let mut kept: Vec<Value> = Vec::new();
    for item in items {
        let mut duplicate = false;
        for other in &kept {
            if same(other, item)? {
                duplicate = true;
                break;
            }
        }
        if !duplicate {
            kept.push(item.clone());
        }
    }
    Ok(kept)
}

fn unique_by<T: Clone>(items: &[T], same: impl Fn(&T, &T) -> bool) -> Vec<T> {
    let mut kept: Vec<T> = Vec::new();
    for item in items {
        if !kept.iter().any(|other| same(other, item)) {
            kept.push(item.clone());
        }
    }
    kept
}

/// The `UnsupportedOperationException` of a tuple or array that rejects a change.
fn rejected(span: Span) -> Error {
    thrown(Exception::new("UnsupportedOperationException", ""), span)
}
