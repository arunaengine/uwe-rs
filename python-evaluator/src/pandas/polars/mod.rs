//! A stand-in for the part of polars that workflows use while they are planned: tables from
//! dicts and CSV files, expressions that select and add columns, and the tables that
//! Snakemake's `validate` checks. The tables use the table core of the pandas stand-in.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod dtype;
mod expr;
mod frame;
mod methods;
mod series;

use std::any::Any;
use std::rc::Rc;

use dtype::DType;
use expr::{Expr, Kind};
use frame::{PolarsFrame, record_names, record_value};
use series::PolarsSeries;

use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::modules::function;
use crate::value::{Args, Native, Value};

pub fn module() -> Vec<(&'static str, Value)> {
    let mut entries = vec![
        ("DataFrame", Value::native(Class::DataFrame)),
        ("Series", Value::native(Class::Series)),
        (
            "col",
            function("col", |interpreter, args| {
                let name = interpreter.to_str(args.get(0).unwrap_or(&Value::None))?;
                Ok(Expr::value(Kind::Column(name)))
            }),
        ),
        (
            "lit",
            function("lit", |_, mut args| {
                let value = args.take(0, "value").unwrap_or(Value::None);
                let literal = Rc::new(Kind::Literal(value));
                Ok(
                    match args.take(1, "dtype").filter(|value| !value.is_none()) {
                        Some(dtype) => Expr::value(Kind::Cast(literal, dtype::of_value(&dtype)?)),
                        None => Expr(literal).into_value(),
                    },
                )
            }),
        ),
    ];
    entries.extend(dtype::dtypes());
    entries
}

/// The classes of the module, which build tables and answer `isinstance`.
enum Class {
    DataFrame,
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
            Class::DataFrame => value.as_native::<PolarsFrame>().is_some(),
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
            Class::DataFrame => data_frame(interpreter, args.take(0, "data"))?,
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

/// `pl.DataFrame(data)`: from a dict of columns, or from a list of dicts, one per row.
fn data_frame(interpreter: &mut Interpreter, data: Option<Value>) -> Result<Value, Error> {
    let data = data.unwrap_or(Value::None);
    let (names, columns) = match data.plain() {
        Value::None => (Vec::new(), Vec::new()),
        Value::Dict(_) => {
            let (mut names, mut columns) = (Vec::new(), Vec::new());
            for (name, values) in interpreter.mapping_items(&data)? {
                names.push(interpreter.to_str(&name)?);
                columns.push(interpreter.iterate(&values)?);
            }
            (names, columns)
        }
        _ => {
            let rows = interpreter.iterate(&data)?;
            if !rows.iter().all(|row| matches!(row.plain(), Value::Dict(_))) {
                return Err(Error::unsupported(
                    "polars.DataFrame from rows that are not dicts",
                ));
            }
            let names = record_names(interpreter, &rows)?;
            let columns = names
                .iter()
                .map(|name| rows.iter().map(|row| record_value(row, name)).collect())
                .collect();
            (names, columns)
        }
    };
    let height = columns.first().map_or(0, Vec::len);
    for (name, column) in names.iter().zip(columns.iter()) {
        if column.len() != height {
            return Err(error(
                "ShapeError",
                format!(
                    "could not create a new DataFrame: height of column '{name}' ({}) does not match height of column '{}' ({height})",
                    column.len(),
                    names[0]
                ),
            ));
        }
    }
    let dtypes = columns.iter().map(|column| DType::of(column)).collect();
    Ok(PolarsFrame::value(names, columns, dtypes))
}

/// An exception of polars, like `ColumnNotFoundError`.
fn error(class: &str, message: impl Into<String>) -> Error {
    let class = crate::types::library_exception(class, "polars.exceptions", "Exception");
    Error::from_value(crate::error::exception(
        &class,
        vec![Value::str(message.into())],
    ))
}
