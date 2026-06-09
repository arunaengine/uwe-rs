//! The methods of polars tables that take expressions and give a new table: `with_columns`
//! and `select`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use super::dtype::DType;
use super::expr::{Expr, Kind, evaluate, output_name};
use super::frame::PolarsFrame;
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Args, Value};

pub const TABLE_METHODS: &[&str] = &["with_columns", "select"];

pub fn table_method(
    interpreter: &mut Interpreter,
    frame: &PolarsFrame,
    name: &str,
    args: Args,
) -> Result<Value, Error> {
    match name {
        "with_columns" => with_columns(interpreter, frame, &args),
        "select" => select(interpreter, frame, &args),
        _ => Err(Error::unsupported(format!("polars {name}"))),
    }
}

/// The expressions of an argument: one expression, a column name, or a list of them.
fn expressions(interpreter: &mut Interpreter, value: &Value) -> Result<Vec<Rc<Kind>>, Error> {
    let column_or_expr = |value: &Value| match value.as_str() {
        Some(name) => Rc::new(Kind::Column(name.to_owned())),
        None => Expr::of(value),
    };
    Ok(match value.plain() {
        Value::List(_) | Value::Tuple(_) => interpreter
            .iterate(value)?
            .iter()
            .map(column_or_expr)
            .collect(),
        _ => vec![column_or_expr(value)],
    })
}

/// `with_columns(*exprs, **named)`: a copy with the column of each expression set.
fn with_columns(
    interpreter: &mut Interpreter,
    frame: &PolarsFrame,
    args: &Args,
) -> Result<Value, Error> {
    let mut exprs: Vec<(Option<String>, Rc<Kind>)> = Vec::new();
    for value in &args.positional {
        for kind in expressions(interpreter, value)? {
            exprs.push((None, kind));
        }
    }
    for (name, value) in &args.keywords {
        exprs.push((Some(name.to_string()), Expr::of(value)));
    }
    let data = frame.data.borrow().clone();
    let copy = frame.copy();
    let Some(table) = copy.as_native::<PolarsFrame>() else {
        return Err(Error::type_error("with_columns() lost its table"));
    };
    for (name, kind) in exprs {
        let values = evaluate(interpreter, &kind, &data)?;
        let name = name
            .or_else(|| output_name(&kind))
            .ok_or_else(|| Error::value_error("an expression without a name"))?;
        table.set_column(&name, values);
    }
    Ok(copy)
}

/// `select(*exprs)`: a table of the columns of the expressions.
fn select(interpreter: &mut Interpreter, frame: &PolarsFrame, args: &Args) -> Result<Value, Error> {
    let data = frame.data.borrow().clone();
    let (mut names, mut columns) = (Vec::new(), Vec::new());
    for value in &args.positional {
        for kind in expressions(interpreter, value)? {
            names.push(output_name(&kind).unwrap_or_default());
            columns.push(evaluate(interpreter, &kind, &data)?);
        }
    }
    let dtypes = columns.iter().map(|column| DType::of(column)).collect();
    Ok(PolarsFrame::value(names, columns, dtypes))
}
