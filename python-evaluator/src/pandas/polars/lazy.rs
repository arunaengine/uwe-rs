//! A polars `LazyFrame`: a table whose methods give other lazy tables until `collect()`.
//! The stand-in holds the table itself, so it reads files when it is made.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;

use super::frame::PolarsFrame;
use super::methods::{TABLE_METHODS, table_method};
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Native, Value};

pub struct LazyFrame(pub Value);

impl LazyFrame {
    pub fn value(frame: Value) -> Value {
        Value::native(LazyFrame(frame))
    }

    /// The table that `collect()` gives.
    pub fn frame(&self) -> Option<&PolarsFrame> {
        self.0.as_native::<PolarsFrame>()
    }
}

impl Native for LazyFrame {
    fn type_name(&self) -> &str {
        "LazyFrame"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        interpreter: &mut Interpreter,
        this: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let this = this.clone();
        let lost = || Error::type_error("the LazyFrame lost its table");
        Ok(Some(match name {
            "collect" => Value::builtin("collect", move |_, _| {
                let lazy = this.as_native::<LazyFrame>().ok_or_else(lost)?;
                Ok(lazy.frame().ok_or_else(lost)?.copy())
            }),
            "columns" => return interpreter.get_attribute(&self.0, "columns").map(Some),
            _ if TABLE_METHODS.contains(&name) => {
                let which = name.to_owned();
                Value::builtin(name, move |interpreter, args| {
                    let lazy = this.as_native::<LazyFrame>().ok_or_else(lost)?;
                    let table = lazy.frame().ok_or_else(lost)?;
                    Ok(LazyFrame::value(table_method(
                        interpreter,
                        table,
                        &which,
                        args,
                    )?))
                })
            }
            _ => return Ok(None),
        }))
    }
}
