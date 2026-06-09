//! The data types of the polars stand-in, like `pl.String` and `pl.Int64`, and the casts of
//! values to them. A missing value is `None`, polars' null.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;

use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::pandas::DtypeKind;
use crate::value::{Native, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DType {
    String,
    Int(&'static str),
    Float(&'static str),
    Boolean,
    Null,
}

impl DType {
    pub fn name(self) -> &'static str {
        match self {
            DType::String => "String",
            DType::Int(name) | DType::Float(name) => name,
            DType::Boolean => "Boolean",
            DType::Null => "Null",
        }
    }

    /// The type that a column of values has.
    pub fn of(values: &[Value]) -> DType {
        let present: Vec<&Value> = values.iter().filter(|value| !value.is_none()).collect();
        if present.is_empty() {
            DType::Null
        } else if present
            .iter()
            .all(|value| matches!(value.plain(), Value::Bool(_)))
        {
            DType::Boolean
        } else if present
            .iter()
            .all(|value| matches!(value.plain(), Value::Int(_)))
        {
            DType::Int("Int64")
        } else if present
            .iter()
            .all(|value| matches!(value.plain(), Value::Int(_) | Value::Float(_)))
        {
            DType::Float("Float64")
        } else {
            DType::String
        }
    }
}

impl Native for DType {
    fn type_name(&self) -> &str {
        "DataTypeClass"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn repr(&self, _: &mut Interpreter) -> Result<String, Error> {
        Ok(self.name().to_owned())
    }
}

/// The data types of the module, by name.
pub fn dtypes() -> Vec<(&'static str, Value)> {
    let mut types = vec![
        ("String", DType::String),
        ("Utf8", DType::String),
        ("Boolean", DType::Boolean),
        ("Null", DType::Null),
        ("Float32", DType::Float("Float32")),
        ("Float64", DType::Float("Float64")),
    ];
    for name in [
        "Int8", "Int16", "Int32", "Int64", "UInt8", "UInt16", "UInt32", "UInt64",
    ] {
        types.push((name, DType::Int(name)));
    }
    types
        .into_iter()
        .map(|(name, dtype)| (name, Value::native(dtype)))
        .collect()
}

/// The type that a value names: a polars type, or a Python type like `str`, as polars
/// accepts them in a schema or a cast.
pub fn of_value(value: &Value) -> Result<DType, Error> {
    if let Some(dtype) = value.as_native::<DType>() {
        return Ok(*dtype);
    }
    Ok(match crate::pandas::dtype_kind(value) {
        Some(DtypeKind::Str) => DType::String,
        Some(DtypeKind::Int) => DType::Int("Int64"),
        Some(DtypeKind::Float) => DType::Float("Float64"),
        Some(DtypeKind::Bool) => DType::Boolean,
        _ => {
            return Err(Error::type_error(format!(
                "cannot parse input of type '{}' into a polars data type",
                crate::types::type_name(value)
            )));
        }
    })
}

/// A value cast to a type, as `cast` and a schema do; null stays null.
pub fn cast(interpreter: &mut Interpreter, value: &Value, dtype: DType) -> Result<Value, Error> {
    if value.is_none() {
        return Ok(Value::None);
    }
    let invalid = |text: &str| {
        super::error(
            "InvalidOperationError",
            format!("conversion from `{text}` to `{}` failed", dtype.name()),
        )
    };
    Ok(match dtype {
        DType::String => Value::str(interpreter.to_str(value)?),
        DType::Null => Value::None,
        DType::Boolean => match value.plain() {
            Value::Bool(flag) => Value::Bool(*flag),
            Value::Str(text) => match text.to_ascii_lowercase().as_str() {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                _ => return Err(invalid(text)),
            },
            other => Value::Bool(interpreter.truthy(other)?),
        },
        DType::Int(name) => {
            let number = match value.plain() {
                Value::Int(number) => *number,
                Value::Bool(flag) => i64::from(*flag),
                Value::Float(number) if number.fract() == 0.0 => *number as i64,
                Value::Str(text) => text.trim().parse::<i64>().map_err(|_| invalid(text))?,
                _ => return Err(invalid(&interpreter.to_str(value)?)),
            };
            if name.starts_with('U') && number < 0 {
                return Err(invalid(&number.to_string()));
            }
            Value::Int(number)
        }
        DType::Float(_) => match value.plain() {
            Value::Int(number) => Value::Float(*number as f64),
            Value::Float(number) => Value::Float(*number),
            Value::Str(text) => {
                Value::Float(text.trim().parse::<f64>().map_err(|_| invalid(text))?)
            }
            _ => return Err(invalid(&interpreter.to_str(value)?)),
        },
    })
}

/// The type of text read from a file: whole numbers, else numbers, else booleans, else texts.
pub fn infer(values: &[Value]) -> DType {
    let texts: Vec<&str> = values.iter().filter_map(Value::as_str).collect();
    if texts.is_empty() {
        return DType::Null;
    }
    if texts.iter().all(|text| text.parse::<i64>().is_ok()) {
        DType::Int("Int64")
    } else if texts.iter().all(|text| text.parse::<f64>().is_ok()) {
        DType::Float("Float64")
    } else if texts
        .iter()
        .all(|text| matches!(text.to_ascii_lowercase().as_str(), "true" | "false"))
    {
        DType::Boolean
    } else {
        DType::String
    }
}
