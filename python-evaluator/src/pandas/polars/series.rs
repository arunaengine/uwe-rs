//! A polars `Series`: one named column of values.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;

use super::dtype::DType;
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Native, Value};

pub struct PolarsSeries {
    pub name: String,
    pub values: Vec<Value>,
}

impl PolarsSeries {
    pub fn value(name: &str, values: Vec<Value>) -> Value {
        Value::native(PolarsSeries {
            name: name.to_owned(),
            values,
        })
    }
}

impl Native for PolarsSeries {
    fn type_name(&self) -> &str {
        "Series"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let values = self.values.clone();
        Ok(Some(match name {
            "name" => Value::str(self.name.as_str()),
            "dtype" => Value::native(DType::of(&self.values)),
            "to_list" => Value::builtin("to_list", move |_, _| Ok(Value::list(values.clone()))),
            "len" => Value::builtin("len", move |_, _| Ok(Value::Int(values.len() as i64))),
            _ => return Ok(None),
        }))
    }

    fn get_item(
        &self,
        _: &mut Interpreter,
        _: &Value,
        key: &Value,
    ) -> Result<Option<Value>, Error> {
        let position = key.as_int().unwrap_or(0);
        let length = self.values.len() as i64;
        let index = if position < 0 {
            length + position
        } else {
            position
        };
        usize::try_from(index)
            .ok()
            .and_then(|index| self.values.get(index).cloned())
            .map(Some)
            .ok_or_else(|| Error::index_error(format!("index {position} is out of bounds")))
    }

    fn iterate(&self, _: &mut Interpreter) -> Result<Option<Vec<Value>>, Error> {
        Ok(Some(self.values.clone()))
    }

    fn length(&self) -> Option<usize> {
        Some(self.values.len())
    }
}
