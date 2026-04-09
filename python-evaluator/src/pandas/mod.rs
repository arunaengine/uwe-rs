//! A stand-in for the part of pandas that workflows use while they are planned: reading
//! sample sheets into tables, and looking values up in them.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Args, Value};

pub fn module() -> Vec<(&'static str, Value)> {
    Vec::new()
}

/// The rows of a data frame as dicts without missing values, or `None` for other values.
pub fn records(
    _interpreter: &mut Interpreter,
    _value: &Value,
) -> Result<Option<Vec<Value>>, Error> {
    Ok(None)
}

/// Fills missing values of a data frame from validated rows, which may have defaults.
pub fn fill_from_records(
    _interpreter: &mut Interpreter,
    _frame: &Value,
    _records: &[Value],
) -> Result<(), Error> {
    Ok(())
}

/// `lookup(query=..., within=frame)`: the rows of a data frame that a query selects.
pub fn lookup_query(
    _interpreter: &mut Interpreter,
    _within: &Value,
    _query: &str,
    _cols: Option<Value>,
    _is_nrows: Option<Value>,
    _default: Option<Value>,
) -> Result<Value, Error> {
    Err(Error::unsupported("lookup() with a pandas query"))
}

/// `snakemake.utils.Paramspace`.
pub fn paramspace(_interpreter: &mut Interpreter, _args: Args) -> Result<Value, Error> {
    Err(Error::unsupported("Paramspace"))
}
