//! Values and errors of the protocol with the Python process, as JSON: plain values as
//! themselves or as a one-key object, like `{"tuple": [...]}`, and other objects as handles,
//! like `{"handle": 7, "type": "SeqRecord", "callable": false}`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use indexmap::IndexSet;
use num_bigint::BigInt;
use serde_json::Value as Json;

use super::{PackageObject, Packages};
use crate::error::Error;
use crate::modules::PathObject;
use crate::types;
use crate::value::{Dict, Key, Value};

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
        types::library_exception(&name, &module, base)
    });
    Error::from_value(crate::error::exception(&class, args))
}
