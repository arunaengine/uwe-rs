//! Stand-ins for `org.json` (`JSONObject`, `JSONArray`, `JSONTokener`). Nextflow releases
//! before 24 shipped it, and the `lib/` classes of older nf-core pipelines use it
//! ([decisions](../../../../docs/decisions.md), item 14).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use nextflow_parser::Span;

use super::super::{AtSpan, Error, Interpreter, thrown};
use crate::exception::Exception;
use crate::value::{JsonOptions, Key, Object, State, Value};

impl Interpreter<'_> {
    /// `new JSONTokener(text or stream)`, `new JSONObject(...)` and `new JSONArray(...)`;
    /// `None` for other classes.
    pub(in crate::interpreter) fn org_json_new(
        &mut self,
        class: &str,
        arguments: &[Value],
        span: Span,
    ) -> Result<Option<Value>, Error> {
        Ok(Some(match (class, arguments) {
            ("JSONTokener", [source]) => {
                let text = match source {
                    Value::Object(object) => match &object.borrow().state {
                        State::Stream(bytes, position) => {
                            let rest = &bytes[(*position).min(bytes.len())..];
                            String::from_utf8_lossy(rest).into_owned()
                        }
                        _ => source.to_string(),
                    },
                    other => other.to_string(),
                };
                object("org.json.JSONTokener", State::Text(text.into()))
            }
            ("JSONObject", []) => json_object(Value::map([])),
            ("JSONObject", [source]) => match source {
                Value::Map(_) => wrap(source),
                other => match wrap(&parse(&source_text(other)).at(span)?) {
                    parsed @ Value::Object(_) if is_json(&parsed, false) => parsed,
                    _ => return Err(json_error("A JSONObject text must begin with '{'", span)),
                },
            },
            ("JSONArray", []) => json_array(Value::list(Vec::new())),
            ("JSONArray", [source]) => match source {
                Value::List(_) | Value::Set(_) => json_array(Value::list(
                    super::elements(source).iter().map(wrap).collect(),
                )),
                other => match wrap(&parse(&source_text(other)).at(span)?) {
                    parsed @ Value::Object(_) if is_json(&parsed, true) => parsed,
                    _ => return Err(json_error("A JSONArray text must start with '['", span)),
                },
            },
            _ => return Ok(None),
        }))
    }

    /// Methods of the stand-ins; `None` for other objects or unknown methods.
    pub(in crate::interpreter) fn org_json_method(
        &mut self,
        object: &Rc<RefCell<Object>>,
        name: &str,
        arguments: &[Value],
        span: Span,
    ) -> Result<Option<Value>, Error> {
        let this = Value::Object(object.clone());
        let state = match &object.borrow().state {
            State::JsonObject(map) => Json::Object(map.clone()),
            State::JsonArray(list) => Json::Array(list.clone()),
            _ => return Ok(None),
        };
        match state {
            Json::Object(map) => self.json_object_method(&this, &map, name, arguments, span),
            Json::Array(list) => self.json_array_method(&this, &list, name, arguments, span),
        }
    }

    fn json_object_method(
        &mut self,
        this: &Value,
        map: &Value,
        name: &str,
        arguments: &[Value],
        span: Span,
    ) -> Result<Option<Value>, Error> {
        let Value::Map(entries) = map else {
            return Ok(None);
        };
        let found = |key: &Value| entries.borrow().entries.get(&Key(text(key))).cloned();
        Ok(Some(match (name, arguments) {
            ("get" | "getAt", [key]) => match found(key) {
                Some(value) => value,
                None => {
                    let message = format!("JSONObject[\"{key}\"] not found.");
                    return Err(json_error(&message, span));
                }
            },
            ("getString" | "getJSONObject" | "getJSONArray", [_]) => {
                return self.json_object_method(this, map, "get", arguments, span);
            }
            ("opt" | "optJSONObject" | "optJSONArray", [key]) => found(key).unwrap_or(Value::Null),
            ("optString", [key]) => found(key).map_or(Value::string(""), |v| text(&v)),
            ("optString", [key, default]) => found(key).unwrap_or_else(|| default.clone()),
            ("has", [key]) => Value::Boolean(found(key).is_some()),
            ("isNull", [key]) => Value::Boolean(matches!(found(key), None | Some(Value::Null))),
            ("put", [key, value]) => {
                entries
                    .borrow_mut()
                    .entries
                    .insert(Key(text(key)), wrap(value));
                this.clone()
            }
            ("remove", [key]) => {
                let removed = entries.borrow_mut().entries.shift_remove(&Key(text(key)));
                removed.unwrap_or(Value::Null)
            }
            ("keySet", []) => Value::set(entries.borrow().entries.keys().map(|k| k.0.clone())),
            ("keys", []) => Value::list(
                entries
                    .borrow()
                    .entries
                    .keys()
                    .map(|k| k.0.clone())
                    .collect(),
            ),
            ("length", []) => Value::integer(entries.borrow().entries.len() as i32),
            ("isEmpty", []) => Value::Boolean(entries.borrow().entries.is_empty()),
            ("toMap", []) => plain(this),
            ("toString", []) => Value::string(self.org_json_text(this, span)?),
            // Groovy iterates any other object as one element, like `json.each { }`.
            _ => {
                let items = [this.clone()];
                return self.iterable_method(this, &items, name, arguments, span);
            }
        }))
    }

    fn json_array_method(
        &mut self,
        this: &Value,
        list: &Value,
        name: &str,
        arguments: &[Value],
        span: Span,
    ) -> Result<Option<Value>, Error> {
        let items = super::elements(list);
        let at = |index: &Value| -> Option<Value> {
            let Value::Number(index) = index else {
                return None;
            };
            usize::try_from(index.to_i64())
                .ok()
                .and_then(|index| items.get(index).cloned())
        };
        Ok(Some(match (name, arguments) {
            ("get" | "getString" | "getJSONObject" | "getJSONArray", [index]) => match at(index) {
                Some(value) => value,
                None => {
                    let message = format!("JSONArray[{index}] not found.");
                    return Err(json_error(&message, span));
                }
            },
            ("opt", [index]) => at(index).unwrap_or(Value::Null),
            ("put", [value]) => {
                if let Value::List(list) = list {
                    list.borrow_mut().items.push(wrap(value));
                }
                this.clone()
            }
            ("length", []) => Value::integer(items.len() as i32),
            ("isEmpty", []) => Value::Boolean(items.is_empty()),
            ("toList", []) => plain(this),
            ("toString", []) => Value::string(self.org_json_text(this, span)?),
            _ => return self.iterable_method(this, &items, name, arguments, span),
        }))
    }

    /// The compact JSON text of a value, with the stand-ins as the maps and lists they hold.
    fn org_json_text(&mut self, value: &Value, span: Span) -> Result<String, Error> {
        self.json_text(&plain(value), &JsonOptions::default(), span)
    }
}

/// What a stand-in holds.
enum Json {
    Object(Value),
    Array(Value),
}

/// Whether an object is a `JSONObject`, whose properties, like `schema.definitions`, Groovy
/// reads with `get(name)`.
pub(in crate::interpreter) fn is_json_object(object: &Rc<RefCell<Object>>) -> bool {
    matches!(object.borrow().state, State::JsonObject(_))
}

/// The items of a `JSONArray`, which `for` loops go through; `None` for other objects.
pub(in crate::interpreter) fn array_items(object: &Rc<RefCell<Object>>) -> Option<Vec<Value>> {
    match &object.borrow().state {
        State::JsonArray(list) => Some(super::elements(list)),
        _ => None,
    }
}

fn object(class: &'static str, state: State) -> Value {
    Value::Object(Rc::new(RefCell::new(Object { class, state })))
}

fn json_object(map: Value) -> Value {
    object("org.json.JSONObject", State::JsonObject(map))
}

fn json_array(list: Value) -> Value {
    object("org.json.JSONArray", State::JsonArray(list))
}

/// Whether a value is a `JSONArray` (`array`) or a `JSONObject`.
fn is_json(value: &Value, array: bool) -> bool {
    let Value::Object(object) = value else {
        return false;
    };
    match &object.borrow().state {
        State::JsonArray(_) => array,
        State::JsonObject(_) => !array,
        _ => false,
    }
}

/// A value as `org.json` holds it: maps become `JSONObject`, lists `JSONArray`.
fn wrap(value: &Value) -> Value {
    match value {
        Value::Map(map) => {
            let entries: Vec<(Value, Value)> = map
                .borrow()
                .entries
                .iter()
                .map(|(key, value)| (text(&key.0), wrap(value)))
                .collect();
            json_object(Value::map(entries))
        }
        Value::List(_) | Value::Set(_) => json_array(Value::list(
            super::elements(value).iter().map(wrap).collect(),
        )),
        other => other.clone(),
    }
}

/// A value with the stand-ins as the Groovy maps and lists they hold, as `toMap()` gives it.
fn plain(value: &Value) -> Value {
    if let Value::Object(object) = value {
        match &object.borrow().state {
            State::JsonObject(map) => return plain(map),
            State::JsonArray(list) => {
                return Value::list(super::elements(list).iter().map(plain).collect());
            }
            _ => {}
        }
    }
    match value {
        Value::Map(map) => Value::map(
            map.borrow()
                .entries
                .iter()
                .map(|(key, value)| (key.0.clone(), plain(value))),
        ),
        Value::List(_) | Value::Set(_) => {
            Value::list(super::elements(value).iter().map(plain).collect())
        }
        other => other.clone(),
    }
}

/// A key as `org.json` keeps it: text.
fn text(key: &Value) -> Value {
    match key {
        Value::String(_) => key.clone(),
        other => Value::string(other.to_string()),
    }
}

/// The JSON text of a source: a `JSONTokener` or text.
fn source_text(source: &Value) -> String {
    if let Value::Object(object) = source
        && let State::Text(text) = &object.borrow().state
    {
        return text.to_string();
    }
    source.to_string()
}

fn parse(text: &str) -> Result<Value, Exception> {
    super::json::parse(text).map_err(|error| Exception::new("JSONException", error.message))
}

fn json_error(message: &str, span: Span) -> Error {
    thrown(Exception::new("JSONException", message), span)
}
