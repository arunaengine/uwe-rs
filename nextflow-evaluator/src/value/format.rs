//! The text of values: Groovy's `FormatHelper.toString` and `write`, and Java's `toString()`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;
use std::rc::Rc;

use super::{ListKind, Value};
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
        match self {
            Value::List(list) if list.peek().range.is_some() => {
                let mut text = String::new();
                write_items(&mut text, list, |item, out| item.write_groovy(out));
                text
            }
            _ => self.to_groovy_string(),
        }
    }

    /// Java's `toString()`, as `String.valueOf` gives it: `{a=1}` for maps and `a=1` for entries.
    pub fn to_java_string(&self) -> String {
        let mut text = String::new();
        self.write_java(&mut text);
        text
    }

    fn write_groovy(&self, out: &mut String) {
        super::deep(|| self.write_groovy_body(out));
    }

    fn write_groovy_body(&self, out: &mut String) {
        match self {
            Value::List(list) if list.peek().range.is_some() => self.write_java(out),
            // The files of one task input, as in a template: their names, separated by blanks.
            Value::List(list) if list.peek().kind == ListKind::BlankSeparated => {
                self.write_java(out)
            }
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
        super::deep(|| self.write_java_body(out));
    }

    fn write_java_body(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Boolean(value) => out.push_str(if *value { "true" } else { "false" }),
            Value::Number(number) => out.push_str(&number.to_string()),
            Value::Character(unit) => out.push_str(&String::from_utf16_lossy(&[*unit])),
            Value::String(text) | Value::GString(text) => out.push_str(text),
            Value::Date(millis) => out.push_str(&crate::date::text(*millis)),
            Value::Class(name) => out.push_str(&super::class_text(name)),
            Value::List(list) => {
                let range = list.peek().range.clone();
                match range {
                    Some(range) => out.push_str(&range.text()),
                    None if list.peek().kind == ListKind::BlankSeparated => {
                        let names: Vec<String> = list
                            .borrow()
                            .items
                            .iter()
                            .map(Value::to_java_string)
                            .collect();
                        out.push_str(&names.join(" "));
                    }
                    None => write_items(out, list, |item, out| item.write_java(out)),
                }
            }
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
            Value::Object(object) => match &object.borrow().state {
                super::State::Uri(text) | super::State::Locale(text) | super::State::Text(text) => {
                    out.push_str(text)
                }
                super::State::Uuid(value) => out.push_str(&value.to_string()),
                super::State::Builder(text) => out.push_str(text),
                super::State::Optional(super::Value::Null) => out.push_str("Optional.empty"),
                super::State::Optional(value) => {
                    out.push_str("Optional[");
                    value.write_java(out);
                    out.push(']');
                }
                super::State::Time(time) => out.push_str(&time.text()),
                super::State::Bytes(bytes) => {
                    let items: Vec<String> = bytes.iter().map(|b| (*b as i8).to_string()).collect();
                    out.push_str(&format!("[{}]", items.join(", ")));
                }
                // `GroupKey.toString()` is Groovy code, so a map key prints as Groovy shows it.
                super::State::GroupKey(target, _) => out.push_str(&target.to_groovy_string()),
                // An enum constant prints its name; a class prints `class Name`, as in Java.
                super::State::EnumConstant { name, .. } => out.push_str(name),
                super::State::Class(class) => {
                    out.push_str(&format!("class {}", class.def.name.replace('.', "$")))
                }
                _ => {
                    let class = object.borrow().class;
                    out.push_str(&format!("{class}@{:x}", Rc::as_ptr(object) as usize));
                }
            },
            Value::Pattern(pattern) => out.push_str(&pattern.source),
            Value::Path(path) => out.push_str(path.text()),
            Value::Channel(id) => out.push_str(&format!("DataflowBroadcast@{}", id.0)),
            Value::ChannelOut(_) => out.push_str("ChannelOut"),
            Value::Definition(definition) => out.push_str(&definition.name),
            Value::Namespace(name) => out.push_str(name),
            Value::Operator(operator) => out.push_str(&operator.0),
            Value::Matcher(matcher) => out.push_str(&format!(
                "java.util.regex.Matcher[pattern={} region=0,{} lastmatch=]",
                matcher.pattern.source,
                matcher.text.encode_utf16().count()
            )),
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
    list: &Rc<crate::value::ListCell>,
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
    map: &Rc<crate::value::MapCell>,
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
