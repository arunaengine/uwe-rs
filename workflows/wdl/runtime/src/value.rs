//! WDL values, their coercion to declared types, and their JSON form.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use indexmap::IndexMap;
use serde_json::{Map as JsonMap, Value as Json};
use wdl_analysis::types::{CompoundType, CustomType, Optional, PrimitiveType, Type};

/// A value of a WDL expression. Files and directories hold a path or an address: outside a
/// task the location of the file, inside a task the path where the task sees it.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    None,
    Boolean(bool),
    Int(i64),
    Float(f64),
    String(String),
    File(String),
    Directory(String),
    Array(Vec<Value>),
    Pair(Box<Value>, Box<Value>),
    /// A map keeps the order of its entries; keys are primitive values.
    Map(Vec<(Value, Value)>),
    /// An object, a struct or the outputs of a call.
    Object(IndexMap<String, Value>),
}

impl Value {
    /// The text of a primitive value, as WDL puts it into a string: a float with six decimals.
    pub fn text(&self) -> Option<String> {
        Some(match self {
            Value::Boolean(value) => value.to_string(),
            Value::Int(value) => value.to_string(),
            Value::Float(value) => format!("{value:.6}"),
            Value::String(text) | Value::File(text) | Value::Directory(text) => text.clone(),
            _ => return None,
        })
    }

    /// The type name of a value, for messages.
    pub fn kind(&self) -> &'static str {
        match self {
            Value::None => "None",
            Value::Boolean(_) => "Boolean",
            Value::Int(_) => "Int",
            Value::Float(_) => "Float",
            Value::String(_) => "String",
            Value::File(_) => "File",
            Value::Directory(_) => "Directory",
            Value::Array(_) => "Array",
            Value::Pair(..) => "Pair",
            Value::Map(_) => "Map",
            Value::Object(_) => "Object",
        }
    }

    /// Calls `visit` on every file and directory path inside the value.
    pub fn visit_paths(&mut self, visit: &mut dyn FnMut(&mut String, bool)) {
        match self {
            Value::File(path) => visit(path, false),
            Value::Directory(path) => visit(path, true),
            Value::Array(items) => items.iter_mut().for_each(|item| item.visit_paths(visit)),
            Value::Pair(left, right) => {
                left.visit_paths(visit);
                right.visit_paths(visit);
            }
            Value::Map(entries) => (entries.iter_mut()).for_each(|(key, value)| {
                key.visit_paths(visit);
                value.visit_paths(visit);
            }),
            Value::Object(fields) => fields.values_mut().for_each(|item| item.visit_paths(visit)),
            _ => {}
        }
    }

    /// The value as the declared type takes it, as WDL coerces values.
    pub fn coerce(self, to: &Type) -> Result<Value, String> {
        let fail = |value: &Value| format!("a {} cannot be a {to}", value.kind());
        match to {
            Type::Union | Type::None | Type::Hidden(_) | Type::Call(_) | Type::TypeNameRef(_) => {
                Ok(self)
            }
            _ if self == Value::None => match to.is_optional() {
                true => Ok(Value::None),
                false => Err(format!("a value of type {to} is required, but it is None")),
            },
            Type::Primitive(primitive, _) => match (primitive, self) {
                (PrimitiveType::Boolean, value @ Value::Boolean(_))
                | (PrimitiveType::Integer, value @ Value::Int(_))
                | (PrimitiveType::Float, value @ Value::Float(_))
                | (PrimitiveType::String, value @ Value::String(_))
                | (PrimitiveType::File, value @ Value::File(_))
                | (PrimitiveType::Directory, value @ Value::Directory(_)) => Ok(value),
                (PrimitiveType::Float, Value::Int(number)) => Ok(Value::Float(number as f64)),
                (PrimitiveType::String, Value::File(text) | Value::Directory(text)) => {
                    Ok(Value::String(text))
                }
                (PrimitiveType::File, Value::String(text)) => Ok(Value::File(text)),
                (PrimitiveType::Directory, Value::String(text)) => Ok(Value::Directory(text)),
                (_, value) => Err(fail(&value)),
            },
            Type::Compound(compound, _) => self.coerce_compound(compound, to),
            Type::Object | Type::OptionalObject => match self {
                value @ Value::Object(_) => Ok(value),
                Value::Map(entries) => Ok(Value::Object(string_keys(entries)?)),
                value => Err(fail(&value)),
            },
        }
    }

    fn coerce_compound(self, compound: &CompoundType, to: &Type) -> Result<Value, String> {
        let fail = |value: &Value| format!("a {} cannot be a {to}", value.kind());
        match (compound, self) {
            (CompoundType::Array(array), Value::Array(items)) => {
                if array.is_non_empty() && items.is_empty() {
                    return Err(format!("an empty array cannot be a {to}"));
                }
                let element = array.element_type();
                (items.into_iter().map(|item| item.coerce(element)))
                    .collect::<Result<_, _>>()
                    .map(Value::Array)
            }
            (CompoundType::Pair(pair), Value::Pair(left, right)) => Ok(Value::Pair(
                Box::new(left.coerce(pair.left_type())?),
                Box::new(right.coerce(pair.right_type())?),
            )),
            (CompoundType::Map(map), Value::Map(entries)) => (entries.into_iter())
                .map(|(key, value)| {
                    Ok((key.coerce(map.key_type())?, value.coerce(map.value_type())?))
                })
                .collect::<Result<_, String>>()
                .map(Value::Map),
            (CompoundType::Map(map), Value::Object(fields)) => (fields.into_iter())
                .map(|(key, value)| {
                    let key = Value::String(key).coerce(map.key_type())?;
                    Ok((key, value.coerce(map.value_type())?))
                })
                .collect::<Result<_, String>>()
                .map(Value::Map),
            (CompoundType::Custom(CustomType::Struct(record)), value) => {
                let mut fields = match value {
                    Value::Object(fields) => fields,
                    Value::Map(entries) => string_keys(entries)?,
                    value => return Err(fail(&value)),
                };
                let mut members = IndexMap::new();
                for (name, member) in record.members() {
                    let field = fields.shift_remove(name).unwrap_or(Value::None);
                    let field = field
                        .coerce(member)
                        .map_err(|error| format!("{name}: {error}"))?;
                    members.insert(name.clone(), field);
                }
                match fields.keys().next() {
                    Some(name) => Err(format!("struct {} has no member `{name}`", record.name())),
                    None => Ok(Value::Object(members)),
                }
            }
            (CompoundType::Custom(CustomType::Enum(_)), _) => {
                Err("enumerations are not supported".to_owned())
            }
            (_, value) => Err(fail(&value)),
        }
    }

    /// The JSON form of a value, as WDL writes outputs: a pair is an object with `left` and
    /// `right`, a map an object of its keys as text.
    pub fn to_json(&self) -> Json {
        match self {
            Value::None => Json::Null,
            Value::Boolean(value) => Json::Bool(*value),
            Value::Int(value) => Json::from(*value),
            Value::Float(value) => serde_json::Number::from_f64(*value)
                .map(Json::Number)
                .unwrap_or(Json::Null),
            Value::String(text) | Value::File(text) | Value::Directory(text) => {
                Json::String(text.clone())
            }
            Value::Array(items) => Json::Array(items.iter().map(Value::to_json).collect()),
            Value::Pair(left, right) => {
                serde_json::json!({"left": left.to_json(), "right": right.to_json()})
            }
            Value::Map(entries) => Json::Object(
                (entries.iter())
                    .map(|(key, value)| (key.text().unwrap_or_default(), value.to_json()))
                    .collect(),
            ),
            Value::Object(fields) => Json::Object(
                (fields.iter())
                    .map(|(name, value)| (name.clone(), value.to_json()))
                    .collect(),
            ),
        }
    }

    /// The value as a result value of uwe: a File or a Directory as an object of its class,
    /// location and basename, a pair as an array of two, the rest as [`Value::to_json`] has it.
    pub fn result_json(&self) -> Json {
        match self {
            Value::File(path) | Value::Directory(path) => {
                let class = match self {
                    Value::File(_) => "File",
                    _ => "Directory",
                };
                let trimmed = path.trim_end_matches('/');
                let basename = trimmed.rsplit('/').next().unwrap_or(trimmed);
                serde_json::json!({"class": class, "location": path, "basename": basename})
            }
            Value::Array(items) => Json::Array(items.iter().map(Value::result_json).collect()),
            Value::Pair(left, right) => Json::Array(vec![left.result_json(), right.result_json()]),
            Value::Map(entries) => Json::Object(
                (entries.iter())
                    .map(|(key, value)| (key.text().unwrap_or_default(), value.result_json()))
                    .collect(),
            ),
            Value::Object(fields) => Json::Object(
                (fields.iter())
                    .map(|(name, value)| (name.clone(), value.result_json()))
                    .collect(),
            ),
            other => other.to_json(),
        }
    }

    /// A value of the declared type from JSON. Relative file paths start at `base`.
    pub fn from_json(json: &Json, to: &Type, base: &Path) -> Result<Value, String> {
        let value = untyped(json, to, base);
        value.coerce(to)
    }
}

/// The value of JSON, read with the declared type where JSON alone does not tell.
fn untyped(json: &Json, to: &Type, base: &Path) -> Value {
    let compound = |to: &Type| match to {
        Type::Compound(compound, _) => Some(compound.clone()),
        _ => None,
    };
    match json {
        Json::Null => Value::None,
        Json::Bool(value) => Value::Boolean(*value),
        Json::Number(number) => match (number.as_i64(), to.as_primitive()) {
            (Some(value), Some(PrimitiveType::Integer) | None) => Value::Int(value),
            _ => Value::Float(number.as_f64().unwrap_or_default()),
        },
        Json::String(text) => match to.as_primitive() {
            Some(PrimitiveType::File) => Value::File(located(text, base)),
            Some(PrimitiveType::Directory) => Value::Directory(located(text, base)),
            Some(PrimitiveType::Integer) => {
                text.parse().map_or(Value::String(text.clone()), Value::Int)
            }
            Some(PrimitiveType::Float) => text
                .parse()
                .map_or(Value::String(text.clone()), Value::Float),
            Some(PrimitiveType::Boolean) => match text.as_str() {
                "true" => Value::Boolean(true),
                "false" => Value::Boolean(false),
                _ => Value::String(text.clone()),
            },
            _ => Value::String(text.clone()),
        },
        Json::Array(items) => {
            let element = match compound(to) {
                Some(CompoundType::Array(array)) => array.element_type().clone(),
                _ => Type::Union,
            };
            Value::Array(
                items
                    .iter()
                    .map(|item| untyped(item, &element, base))
                    .collect(),
            )
        }
        Json::Object(fields) => object(fields, compound(to), base),
    }
}

/// The value of a JSON object: a pair, a map, a struct or an object, by the declared type.
fn object(fields: &JsonMap<String, Json>, to: Option<CompoundType>, base: &Path) -> Value {
    match to {
        Some(CompoundType::Pair(pair)) => Value::Pair(
            Box::new(untyped(
                fields.get("left").unwrap_or(&Json::Null),
                pair.left_type(),
                base,
            )),
            Box::new(untyped(
                fields.get("right").unwrap_or(&Json::Null),
                pair.right_type(),
                base,
            )),
        ),
        Some(CompoundType::Map(map)) => Value::Map(
            (fields.iter())
                .map(|(key, value)| {
                    let key = untyped(&Json::String(key.clone()), map.key_type(), base);
                    (key, untyped(value, map.value_type(), base))
                })
                .collect(),
        ),
        Some(CompoundType::Custom(CustomType::Struct(record))) => Value::Object(
            (fields.iter())
                .map(|(name, value)| {
                    let member = record.members().get(name).cloned().unwrap_or(Type::Union);
                    (name.clone(), untyped(value, &member, base))
                })
                .collect(),
        ),
        _ => Value::Object(
            (fields.iter())
                .map(|(name, value)| (name.clone(), untyped(value, &Type::Union, base)))
                .collect(),
        ),
    }
}

/// An absolute path for a relative one; addresses like `s3://` stay as they are.
pub fn located(text: &str, base: &Path) -> String {
    if text.contains("://") || Path::new(text).is_absolute() {
        return text.to_owned();
    }
    base.join(text).to_string_lossy().into_owned()
}

/// The fields of a map whose keys are strings.
fn string_keys(entries: Vec<(Value, Value)>) -> Result<IndexMap<String, Value>, String> {
    (entries.into_iter())
        .map(|(key, value)| match key {
            Value::String(name) => Ok((name, value)),
            key => Err(format!(
                "a map with {} keys cannot be an object",
                key.kind()
            )),
        })
        .collect()
}

#[cfg(test)]
#[path = "value_tests.rs"]
mod tests;
