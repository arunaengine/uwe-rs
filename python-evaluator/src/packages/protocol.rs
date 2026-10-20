//! Encodes Python process values, object handles, and errors as JSON protocol messages.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use indexmap::IndexSet;
use num_bigint::BigInt;
use serde_json::{Map, Value as Json};

use super::{PackageObject, Packages};
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::modules::PathObject;
use crate::types;
use crate::value::{Dict, Key, Value};

fn tagged(tag: &str, value: Json) -> Json {
    let mut object = Map::new();
    object.insert(tag.to_owned(), value);
    Json::Object(object)
}

/// A value of the interpreter as the process takes it. Functions, classes and objects of the
/// stand-ins cannot cross, which is unsupported.
pub fn encode(interpreter: &mut Interpreter, value: &Value) -> Result<Json, Error> {
    let all = |interpreter: &mut Interpreter, items: &[Value]| -> Result<Json, Error> {
        let mut encoded = Vec::with_capacity(items.len());
        for item in items {
            encoded.push(encode(interpreter, item)?);
        }
        Ok(Json::Array(encoded))
    };
    Ok(match value.plain() {
        Value::None => Json::Null,
        Value::Bool(flag) => Json::Bool(*flag),
        Value::Int(number) => Json::from(*number),
        Value::BigInt(number) => tagged("int", Json::String(number.to_string())),
        Value::Float(number) => tagged("float", float(*number)),
        Value::Str(text) => Json::String(text.to_string()),
        Value::Bytes(bytes) => tagged(
            "bytes",
            Json::String(bytes.iter().map(|byte| format!("{byte:02x}")).collect()),
        ),
        Value::List(items) => {
            let items = items.borrow().clone();
            tagged("list", all(interpreter, &items)?)
        }
        Value::Tuple(items) => tagged("tuple", all(interpreter, items)?),
        Value::Set(_) | Value::FrozenSet(_) | Value::Range(_) | Value::Iterator(_) => {
            let items = interpreter.iterate(value)?;
            let tag = if matches!(value.plain(), Value::Set(_) | Value::FrozenSet(_)) {
                "set"
            } else {
                "list"
            };
            tagged(tag, all(interpreter, &items)?)
        }
        Value::Dict(dict) => {
            let entries: Vec<(Value, Value)> = dict
                .borrow()
                .entries
                .iter()
                .map(|(key, value)| (key.0.clone(), value.clone()))
                .collect();
            let mut pairs = Vec::with_capacity(entries.len());
            for (key, value) in entries {
                pairs.push(Json::Array(vec![
                    encode(interpreter, &key)?,
                    encode(interpreter, &value)?,
                ]));
            }
            tagged("dict", Json::Array(pairs))
        }
        Value::Native(object) => {
            if let Some(object) = object.as_any().downcast_ref::<PackageObject>() {
                object.reference()?
            } else if let Some(path) = object.as_any().downcast_ref::<PathObject>() {
                tagged("path", Json::String(path.path.clone()))
            } else if let Some(record) = object.as_any().downcast_ref::<super::Record>() {
                let mut names = Vec::with_capacity(record.names.len());
                for (name, item) in &record.names {
                    names.push(Json::Array(vec![
                        Json::String(name.clone()),
                        encode(interpreter, item)?,
                    ]));
                }
                let items = all(interpreter, &record.items)?;
                tagged("record", Json::Array(vec![items, Json::Array(names)]))
            } else if object.instance_of("str") {
                Json::String(interpreter.to_str(value)?)
            } else if let Ok(items) = interpreter.iterate(value) {
                // Lists of the host, like Snakemake's lists of files.
                tagged("list", all(interpreter, &items)?)
            } else {
                return Err(cannot_cross(value));
            }
        }
        _ => return Err(cannot_cross(value)),
    })
}

fn cannot_cross(value: &Value) -> Error {
    Error::unsupported(format!(
        "passing a {} of the workflow to a Python package, which runs in the container of decision 19",
        types::type_name(value)
    ))
}

fn float(number: f64) -> Json {
    if number.is_nan() {
        Json::String("nan".into())
    } else if number.is_infinite() {
        Json::String(if number > 0.0 { "inf" } else { "-inf" }.into())
    } else {
        Json::from(number)
    }
}

/// A value that the process sent: a plain value, or a handle of its object.
pub fn decode(packages: &Rc<Packages>, json: &Json) -> Result<Value, Error> {
    let all = |items: &Json| -> Result<Vec<Value>, Error> {
        match items {
            Json::Array(items) => items.iter().map(|item| decode(packages, item)).collect(),
            _ => Err(malformed(items)),
        }
    };
    Ok(match json {
        Json::Null => Value::None,
        Json::Bool(flag) => Value::Bool(*flag),
        Json::Number(number) => match number.as_i64() {
            Some(number) => Value::Int(number),
            None => Value::Float(number.as_f64().unwrap_or(f64::NAN)),
        },
        Json::String(text) => Value::str(text.as_str()),
        Json::Array(_) => Value::list(all(json)?),
        Json::Object(object) if object.contains_key("handle") => {
            PackageObject::value(packages, object)?
        }
        Json::Object(object) => {
            let Some((tag, inner)) = object.iter().next() else {
                return Err(malformed(json));
            };
            match tag.as_str() {
                "int" => {
                    let digits = inner.as_str().unwrap_or_default();
                    let number: BigInt = digits.parse().map_err(|_| malformed(json))?;
                    Value::from_bigint(number)
                }
                "float" => Value::Float(match inner {
                    Json::String(text) => match text.as_str() {
                        "inf" => f64::INFINITY,
                        "-inf" => f64::NEG_INFINITY,
                        _ => f64::NAN,
                    },
                    number => number.as_f64().ok_or_else(|| malformed(json))?,
                }),
                "bytes" => {
                    let hex = inner.as_str().unwrap_or_default();
                    let bytes: Option<Vec<u8>> = (0..hex.len())
                        .step_by(2)
                        .map(|at| {
                            hex.get(at..at + 2)
                                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                        })
                        .collect();
                    Value::Bytes(bytes.ok_or_else(|| malformed(json))?.into())
                }
                "list" => Value::list(all(inner)?),
                "tuple" => Value::tuple(all(inner)?),
                "set" => {
                    let mut set = IndexSet::new();
                    for item in all(inner)? {
                        set.insert(Key::new(item)?);
                    }
                    Value::set(set)
                }
                "dict" => {
                    let mut dict = Dict::default();
                    for pair in all(inner)? {
                        let Value::List(pair) = pair else {
                            return Err(malformed(json));
                        };
                        let pair = pair.borrow();
                        let [key, value] = pair.as_slice() else {
                            return Err(malformed(json));
                        };
                        dict.entries.insert(Key::new(key.clone())?, value.clone());
                    }
                    Value::dict(dict)
                }
                "path" => PathObject::value(inner.as_str().unwrap_or_default()),
                _ => return Err(malformed(json)),
            }
        }
    })
}

fn malformed(json: &Json) -> Error {
    Error::stop(format!(
        "the Python process of the packages sent a value that iunctus does not know: {json}"
    ))
}

/// The attribute of a class of exceptions of a package that names it and its bases, like
/// `pyfaidx.FastaIndexingError`, for `except` clauses with classes of the package.
pub const CLASSES: &str = "__package_classes__";

/// The exception of an error reply: its builtin class, or a class of the package whose base
/// is the nearest builtin class among its bases.
pub fn exception(error: &Json) -> Error {
    let text = |name: &str| {
        error
            .get(name)
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let name = text("type");
    let module = text("module");
    let message = text("message");
    let bases: Vec<String> = error
        .get("bases")
        .and_then(Json::as_array)
        .map(|bases| {
            bases
                .iter()
                .filter_map(|base| base.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let args: Vec<Value> = match error.get("args").and_then(Json::as_array) {
        Some(args) => args
            .iter()
            .map(|arg| match arg {
                Json::String(text) => Value::str(text.as_str()),
                Json::Number(number) => number
                    .as_i64()
                    .map_or_else(|| Value::Float(number.as_f64().unwrap_or(0.0)), Value::Int),
                Json::Bool(flag) => Value::Bool(*flag),
                _ => Value::None,
            })
            .collect(),
        None => vec![Value::str(message.as_str())],
    };
    let class = if module == "builtins" {
        types::exception_class(&name)
    } else {
        None
    };
    let class = class.unwrap_or_else(|| {
        let base = bases
            .iter()
            .find(|base| types::exception_class(base).is_some())
            .map_or("Exception", String::as_str);
        let class = types::library_exception(&name, &module, base);
        let classes: Vec<Value> = error
            .get("classes")
            .and_then(Json::as_array)
            .map(|classes| {
                classes
                    .iter()
                    .filter_map(Json::as_str)
                    .map(Value::str)
                    .collect()
            })
            .unwrap_or_default();
        class
            .dict
            .borrow_mut()
            .insert(CLASSES.into(), Value::tuple(classes));
        class
    });
    Error::from_value(crate::error::exception(&class, args))
}
