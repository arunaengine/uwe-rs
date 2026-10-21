//! Trees of nested maps with string keys that keep their first position, like the config and
//! `params`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use super::{Key, List, Map, Value};

/// A new empty map.
pub fn map() -> Value {
    Value::map([])
}

/// The value at `path` below `tree`, if every step is a map that has the key.
pub fn get(tree: &Value, path: &[&str]) -> Option<Value> {
    let mut current = tree.clone();
    for name in path {
        let next = match &current {
            Value::Map(map) => map.borrow().entries.get(&key(name)).cloned(),
            _ => None,
        };
        current = next?;
    }
    Some(current)
}

/// Sets `path` to `value`, creating the maps on the way. A later write replaces the value,
/// which keeps its position. Writing below a value that is not a map is an error.
pub fn assign(tree: &Value, path: &[Rc<str>], value: Value) -> Result<(), String> {
    let (last, parents) = path.split_last().expect("a setting has a name");
    let mut current = tree.clone();
    for (index, name) in parents.iter().enumerate() {
        let Value::Map(scope) = &current else {
            unreachable!("only maps are walked")
        };
        let existing = scope.borrow().entries.get(&key(name)).cloned();
        current = match existing {
            Some(next @ Value::Map(_)) => next,
            Some(_) => {
                return Err(format!(
                    "Cannot set `{}`, because `{}` is not a map",
                    path.join("."),
                    parents[..=index].join(".")
                ));
            }
            None => {
                let next = map();
                scope.borrow_mut().entries.insert(key(name), next.clone());
                next
            }
        };
    }
    let Value::Map(scope) = &current else {
        unreachable!("only maps are walked")
    };
    scope.borrow_mut().entries.insert(key(last), value);
    Ok(())
}

/// Merges `source` into `target`: maps merge key by key, other values replace. Keys that
/// exist keep their position, new keys are appended.
pub fn deep_merge(target: &Value, source: &Value) {
    let (Value::Map(target), Value::Map(source)) = (target, source) else {
        return;
    };
    let entries: Vec<(Key, Value)> = source
        .borrow()
        .entries
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    for (key, value) in entries {
        let existing = target.borrow().entries.get(&key).cloned();
        match (existing, &value) {
            (Some(existing @ Value::Map(_)), Value::Map(_)) => deep_merge(&existing, &value),
            _ => {
                target.borrow_mut().entries.insert(key, deep_copy(&value));
            }
        }
    }
}

/// A copy of nested maps and lists, so that no file shares them with another.
pub fn deep_copy(value: &Value) -> Value {
    super::deep(|| deep_copy_body(value))
}

fn deep_copy_body(value: &Value) -> Value {
    match value {
        Value::Map(map) => {
            let map = map.borrow();
            let entries = map
                .entries
                .iter()
                .map(|(key, value)| (key.clone(), deep_copy(value)))
                .collect();
            Value::Map(Rc::new(RefCell::new(Map {
                kind: map.kind,
                entries,
                default: None,
            })))
        }
        Value::List(list) => {
            let list = list.borrow();
            let items = list.items.iter().map(deep_copy).collect();
            Value::List(Rc::new(RefCell::new(List {
                kind: list.kind,
                items,
                range: list.range.clone(),
                default: list.default.clone(),
            })))
        }
        other => other.clone(),
    }
}

pub fn key(name: &str) -> Key {
    Key(Value::string(name))
}
