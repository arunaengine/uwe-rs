//! The data types of the polars stand-in, like `pl.String` and `pl.Int64`, and the casts of
//! values to them. A missing value is `None`, polars' null.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;

use crate::error::Error;
use crate::interpreter::Interpreter;
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
