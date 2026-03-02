//! The text of values: Groovy's `FormatHelper.toString` and `write`, and Java's `toString()`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use super::Value;
use crate::units::{duration_text, memory_text};

impl Value {
    /// Groovy's `FormatHelper.toString`, used by `+` on strings, `join`, `println`, `as String`
    /// and for the elements of lists and maps: `[1, a]`, `[id:S1]` and `[:]`.
    pub fn to_groovy_string(&self) -> String {
        let mut text = String::new();
        self.write_groovy(&mut text);
        text
    }

    /// Groovy's `FormatHelper.write`, used for a value inserted into a GString. It differs
    /// from `to_groovy_string` for ranges, which it writes as lists.
    pub fn to_interpolated(&self) -> String {
        self.to_groovy_string()
    }

    /// Java's `toString()`, as `String.valueOf` gives it: `{a=1}` for maps and `a=1` for entries.
    pub fn to_java_string(&self) -> String {
        let mut text = String::new();
        self.write_java(&mut text);
        text
    }

    fn write_groovy(&self, out: &mut String) {
        match self {
            Value::List(list) => write_items(out, list, |item, out| item.write_groovy(out)),
            Value::Set(set) => {
                out.push('[');
                for (index, item) in set.borrow().items.iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    item.0.write_groovy(out);
                }
                out.push(']');
            }
            Value::Map(map) => {
                let map_ref = map;
                let map = map.borrow();
                if map.entries.is_empty() {
                    out.push_str("[:]");
                    return;
                }
                out.push('[');
                for (index, (key, value)) in map.entries.iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    write_map_part(out, &key.0, map_ref, Value::write_groovy);
                    out.push(':');
                    write_map_part(out, value, map_ref, Value::write_groovy);
                }
                out.push(']');
            }
            other => other.write_java(out),
        }
    }

    fn write_java(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Boolean(value) => out.push_str(if *value { "true" } else { "false" }),
            Value::Number(number) => out.push_str(&number.to_string()),
            Value::String(text) | Value::GString(text) => out.push_str(text),
            Value::List(list) => write_items(out, list, |item, out| item.write_java(out)),
            Value::Set(set) => {
                out.push('[');
                for (index, item) in set.borrow().items.iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    item.0.write_java(out);
                }
                out.push(']');
            }
            Value::Map(map) => {
                let map_ref = map;
                out.push('{');
                for (index, (key, value)) in map.borrow().entries.iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    write_map_part(out, &key.0, map_ref, Value::write_java);
                    out.push('=');
                    write_map_part(out, value, map_ref, Value::write_java);
                }
                out.push('}');
            }
            Value::MapEntry(entry) => {
                entry.0.write_java(out);
                out.push('=');
                entry.1.write_java(out);
            }
            Value::MemoryUnit(size) => out.push_str(&memory_text(*size)),
            Value::Duration(millis) => out.push_str(&duration_text(*millis)),
            Value::Closure(closure) => {
                out.push_str(&format!("Closure@{:x}", Rc::as_ptr(closure) as usize));
            }
            // Java's `Throwable.toString()`.
            Value::Exception(exception) => {
                out.push_str(exception.qualified_class());
                if !exception.message.is_empty() {
                    out.push_str(": ");
                    out.push_str(&exception.message);
                }
            }
        }
    }
}

/// Writes `[a, b]`, with `(this Collection)` for an element that is the list itself.
fn write_items(
    out: &mut String,
    list: &Rc<RefCell<super::List>>,
    write: impl Fn(&Value, &mut String),
) {
    out.push('[');
    for (index, item) in list.borrow().items.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        match item {
            Value::List(inner) if Rc::ptr_eq(inner, list) => out.push_str("(this Collection)"),
            item => write(item, out),
        }
    }
    out.push(']');
}

/// Writes a key or value of a map, with `(this Map)` for the map itself.
fn write_map_part(
    out: &mut String,
    part: &Value,
    map: &Rc<RefCell<super::Map>>,
    write: fn(&Value, &mut String),
) {
    match part {
        Value::Map(inner) if Rc::ptr_eq(inner, map) => out.push_str("(this Map)"),
        part => write(part, out),
    }
}

/// Groovy's `toString` form, as `FormatHelper.toString` gives it.
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_groovy_string())
    }
}
