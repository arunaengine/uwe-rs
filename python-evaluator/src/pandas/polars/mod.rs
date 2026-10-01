//! Provides polars CSV tables, expressions, and validation using the pandas table core.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod dtype;
mod expr;
mod frame;
mod lazy;
mod methods;
mod series;

use std::any::Any;
use std::rc::Rc;

use dtype::DType;
use expr::{Expr, Kind};
use frame::{PolarsFrame, record_names, record_value};
use lazy::LazyFrame;
use series::PolarsSeries;

use super::{DataFrame, flag, is_missing};
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::modules::function;
use crate::value::{Args, Native, Value};

pub fn module() -> Vec<(&'static str, Value)> {
    let mut entries = vec![
        ("DataFrame", Value::native(Class::DataFrame)),
        ("LazyFrame", Value::native(Class::LazyFrame)),
        ("Series", Value::native(Class::Series)),
        ("read_csv", function("read_csv", read_csv)),
        (
            "scan_csv",
            function("scan_csv", |interpreter, args| {
                Ok(LazyFrame::value(read_csv(interpreter, args)?))
            }),
        ),
        ("concat", function("concat", concat)),
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
    LazyFrame,
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
            Class::LazyFrame => value.as_native::<LazyFrame>().is_some(),
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
            Class::LazyFrame => LazyFrame::value(data_frame(interpreter, args.take(0, "data"))?),
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

/// `pl.read_csv(source, separator=",", has_header=True, schema=None, schema_overrides=None,
/// null_values=None)`. Empty fields and the null values become nulls; the other fields
/// take the type of the schema, or the type that all fields of their column fit.
fn read_csv(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let source = args.take(0, "source").ok_or_else(|| {
        Error::type_error("read_csv() missing 1 required positional argument: 'source'")
    })?;
    let separator = args
        .take_keyword("separator")
        .unwrap_or_else(|| Value::str(","));
    let has_header = flag(interpreter, &mut args, "has_header", true)?;
    let schema = args.take_keyword("schema").filter(|value| !value.is_none());
    let overrides = args
        .take_keyword("schema_overrides")
        .or_else(|| args.take_keyword("dtypes"))
        .filter(|value| !value.is_none());
    let null_values = args
        .take_keyword("null_values")
        .filter(|value| !value.is_none());
    // Read every field as text; empty fields are the only missing ones.
    let mut read_args = Args::new(vec![source]);
    read_args.keywords = vec![
        (Rc::from("sep"), separator),
        (Rc::from("dtype"), Value::str("str")),
        (Rc::from("keep_default_na"), Value::Bool(false)),
        (Rc::from("na_values"), Value::list(vec![Value::str("")])),
    ];
    if !has_header {
        read_args.keywords.push((Rc::from("header"), Value::None));
    }
    let table = match super::read::read(interpreter, read_args, ",") {
        Err(failure) if failure.is("EmptyDataError") => {
            return Err(error("NoDataError", "empty CSV"));
        }
        result => result?,
    };
    let data = table
        .as_native::<DataFrame>()
        .map(DataFrame::data)
        .ok_or_else(|| Error::type_error("read_csv() did not read a table"))?;
    let mut names = Vec::with_capacity(data.columns.len());
    for (position, label) in data.columns.values.iter().enumerate() {
        names.push(if has_header {
            interpreter.to_str(label)?
        } else {
            format!("column_{}", position + 1)
        });
    }
    let mut dtypes: Vec<Option<DType>> = vec![None; names.len()];
    if let Some(schema) = &schema {
        for (position, (name, dtype)) in interpreter.mapping_items(schema)?.into_iter().enumerate()
        {
            if position < names.len() {
                names[position] = interpreter.to_str(&name)?;
                dtypes[position] = Some(dtype::of_value(&dtype)?);
            }
        }
    }
    if let Some(overrides) = &overrides {
        for (name, dtype) in interpreter.mapping_items(overrides)? {
            let name = interpreter.to_str(&name)?;
            if let Some(position) = names.iter().position(|known| *known == name) {
                dtypes[position] = Some(dtype::of_value(&dtype)?);
            }
        }
    }
    let mut columns = Vec::with_capacity(names.len());
    let mut types = Vec::with_capacity(names.len());
    for (position, column) in data.data.into_iter().enumerate() {
        let nulls = match &null_values {
            Some(null_values) => null_texts(interpreter, null_values, &names[position])?,
            None => Vec::new(),
        };
        let texts: Vec<Value> = column
            .into_iter()
            .map(|value| match value.as_str() {
                Some(text) if nulls.iter().any(|null| null == text) => Value::None,
                _ if is_missing(&value) => Value::None,
                _ => value,
            })
            .collect();
        let dtype = dtypes[position].unwrap_or_else(|| dtype::infer(&texts));
        let mut values = Vec::with_capacity(texts.len());
        for text in &texts {
            values.push(dtype::cast(interpreter, text, dtype)?);
        }
        columns.push(values);
        types.push(dtype);
    }
    Ok(PolarsFrame::value(names, columns, types))
}

/// The texts that `null_values` makes null in a column: one text, a list, or a dict by column.
fn null_texts(
    interpreter: &mut Interpreter,
    null_values: &Value,
    column: &str,
) -> Result<Vec<String>, Error> {
    let values = match null_values.plain() {
        Value::Str(text) => return Ok(vec![text.to_string()]),
        Value::Dict(_) => match record_value(null_values, column) {
            Value::None => return Ok(Vec::new()),
            Value::Str(text) => return Ok(vec![text.to_string()]),
            other => interpreter.iterate(&other)?,
        },
        _ => interpreter.iterate(null_values)?,
    };
    values
        .iter()
        .map(|value| interpreter.to_str(value))
        .collect()
}

/// `pl.concat(items)`: the rows of tables with the same columns, one table after the other.
fn concat(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let items = args.take(0, "items").unwrap_or(Value::None);
    let items = interpreter.iterate(&items)?;
    if items.is_empty() {
        return Err(Error::value_error("cannot concat empty list"));
    }
    let lazy = items[0].as_native::<LazyFrame>().is_some();
    let mut frames = Vec::with_capacity(items.len());
    for item in &items {
        let frame = match item.as_native::<LazyFrame>() {
            Some(lazy) => lazy.frame(),
            None => item.as_native::<PolarsFrame>(),
        };
        frames.push(
            frame.ok_or_else(|| {
                Error::type_error("concat() takes polars DataFrames or LazyFrames")
            })?,
        );
    }
    let names = frames[0].names();
    let mut dtypes = frames[0].dtypes.borrow().clone();
    let mut columns: Vec<Vec<Value>> = vec![Vec::new(); names.len()];
    for frame in &frames {
        if frame.names() != names {
            return Err(error(
                "ShapeError",
                "unable to vertically concatenate tables with different column names",
            ));
        }
        let data = frame.data.borrow();
        for (position, column) in data.data.iter().enumerate() {
            columns[position].extend(column.iter().cloned());
            if dtypes[position] == DType::Null {
                dtypes[position] = frame.dtypes.borrow()[position];
            }
        }
    }
    let frame = PolarsFrame::value(names, columns, dtypes);
    Ok(if lazy { LazyFrame::value(frame) } else { frame })
}

/// An exception of polars, like `ColumnNotFoundError`.
fn error(class: &str, message: impl Into<String>) -> Error {
    let class = crate::types::library_exception(class, "polars.exceptions", "Exception");
    Error::from_value(crate::error::exception(
        &class,
        vec![Value::str(message.into())],
    ))
}

/// The rows of a polars table as Snakemake's `validate` checks them: dicts without nulls,
/// and for a `LazyFrame` only its first 1000 rows. `None` for other values.
pub fn records(value: &Value) -> Option<Vec<Value>> {
    let (frame, limit) = match value.as_native::<LazyFrame>() {
        Some(lazy) => (lazy.frame()?, 1000),
        None => (value.as_native::<PolarsFrame>()?, usize::MAX),
    };
    Some(
        frame
            .records()
            .into_iter()
            .take(limit)
            .map(|record| Value::dict_of(record.into_iter().filter(|(_, value)| !value.is_none())))
            .collect(),
    )
}

/// Takes the defaults of validated rows into a polars `DataFrame`, as Snakemake does: the
/// columns that only the rows have are added, and each null cell takes the value at the same
/// row and column position of the table of the rows. A `LazyFrame` stays as it is.
pub fn fill(interpreter: &mut Interpreter, value: &Value, records: &[Value]) -> Result<(), Error> {
    let Some(frame) = value.as_native::<PolarsFrame>() else {
        return Ok(());
    };
    let names = record_names(interpreter, records)?;
    let known = frame.names();
    let rows = frame.data.borrow().rows();
    for name in names.iter().filter(|name| !known.contains(name)) {
        frame.set_column(name, vec![Value::None; rows]);
    }
    let mut data = frame.data.borrow_mut();
    let width = data.data.len();
    for (row, record) in records.iter().enumerate().take(rows) {
        for (values, name) in data.data.iter_mut().zip(&names) {
            if values[row].is_none() {
                values[row] = record_value(record, name);
            }
        }
    }
    let mut dtypes = frame.dtypes.borrow_mut();
    for column in known.len()..width {
        dtypes[column] = DType::of(&data.data[column]);
    }
    Ok(())
}
