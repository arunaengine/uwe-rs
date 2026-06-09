//! A stand-in for the part of polars that workflows use while they are planned: tables from
//! dicts and CSV files, expressions that select and add columns, and the tables that
//! Snakemake's `validate` checks. The tables use the table core of the pandas stand-in.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod dtype;
mod series;

use std::any::Any;

use series::PolarsSeries;

use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Args, Native, Value};

pub fn module() -> Vec<(&'static str, Value)> {
    let mut entries = vec![("Series", Value::native(Class::Series))];
    entries.extend(dtype::dtypes());
    entries
}

/// The classes of the module, which build tables and answer `isinstance`.
enum Class {
    Series,
}

impl Native for Class {
    fn type_name(&self) -> &str {
        "type"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn instance_of(&self, class: &str) -> bool {
        class == "callable"
    }

    fn check_instance(&self, value: &Value) -> Option<bool> {
        Some(match self {
            Class::Series => value.as_native::<PolarsSeries>().is_some(),
        })
    }

    fn call(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        mut args: Args,
    ) -> Result<Option<Value>, Error> {
        Ok(Some(match self {
            Class::Series => {
                let first = args.take(0, "name").unwrap_or(Value::None);
                let (name, values) = match first.as_str() {
                    Some(name) => (name.to_owned(), args.take(1, "values")),
                    None => (String::new(), Some(first)),
                };
                let values = match values.filter(|value| !value.is_none()) {
                    Some(values) => interpreter.iterate(&values)?,
                    None => Vec::new(),
                };
                PolarsSeries::value(&name, values)
            }
        }))
    }
}
