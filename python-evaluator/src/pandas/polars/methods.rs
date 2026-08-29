//! The methods of polars tables that take expressions and give a new table: `with_columns`,
//! `filter`, `sort`, `select` and `head`. A `DataFrame` and a `LazyFrame` share them.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cmp::Ordering;
use std::rc::Rc;

use super::super::flag;
use super::dtype::DType;
use super::expr::{Expr, Kind, evaluate, output_name};
use super::frame::PolarsFrame;
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Args, Value};

pub const TABLE_METHODS: &[&str] = &["with_columns", "filter", "sort", "select", "head", "limit"];

pub fn table_method(
    interpreter: &mut Interpreter,
    frame: &PolarsFrame,
    name: &str,
    args: Args,
) -> Result<Value, Error> {
    match name {
        "with_columns" => with_columns(interpreter, frame, &args),
        "filter" => filter(interpreter, frame, &args),
        "sort" => sort(interpreter, frame, args),
        "select" => select(interpreter, frame, &args),
        "head" | "limit" => {
            let count = args.get(0).and_then(Value::as_int).unwrap_or(5).max(0) as usize;
            let rows: Vec<usize> = (0..frame.data.borrow().rows().min(count)).collect();
            Ok(frame.take_rows(&rows))
        }
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

/// `filter(*predicates)`: the rows for which every predicate is true.
fn filter(interpreter: &mut Interpreter, frame: &PolarsFrame, args: &Args) -> Result<Value, Error> {
    let data = frame.data.borrow().clone();
    let mut keep = vec![true; data.rows()];
    for value in &args.positional {
        for kind in expressions(interpreter, value)? {
            for (row, truth) in evaluate(interpreter, &kind, &data)?.iter().enumerate() {
                keep[row] &= matches!(truth.plain(), Value::Bool(true));
            }
        }
    }
    for (name, value) in &args.keywords {
        let column = data
            .column_position(&Value::str(name.to_string()))
            .ok_or_else(|| Error::new("ColumnNotFoundError", name.to_string()))?;
        for (row, item) in data.data[column].iter().enumerate() {
            keep[row] &= !item.is_none() && !value.is_none() && interpreter.equals(item, value)?;
        }
    }
    let rows: Vec<usize> = (0..data.rows()).filter(|row| keep[*row]).collect();
    Ok(frame.take_rows(&rows))
}

/// `sort(by, descending=False, nulls_last=False)`: the rows ordered by columns.
fn sort(
    interpreter: &mut Interpreter,
    frame: &PolarsFrame,
    mut args: Args,
) -> Result<Value, Error> {
    let by = args.take(0, "by").unwrap_or(Value::None);
    let descending = flag(interpreter, &mut args, "descending", false)?;
    let null = if flag(interpreter, &mut args, "nulls_last", false)? {
        Ordering::Greater
    } else {
        Ordering::Less
    };
    let data = frame.data.borrow().clone();
    let mut keys = Vec::new();
    for kind in expressions(interpreter, &by)? {
        keys.push(evaluate(interpreter, &kind, &data)?);
    }
    let mut rows: Vec<usize> = (0..data.rows()).collect();
    let mut failure = None;
    rows.sort_by(|a, b| {
        for key in &keys {
            let (x, y) = (&key[*a], &key[*b]);
            let order = match (x.is_none(), y.is_none()) {
                (true, true) => Ordering::Equal,
                (true, false) => return null,
                (false, true) => return null.reverse(),
                _ => match interpreter.compare(x, y) {
                    Ok(order) => order,
                    Err(error) => {
                        failure.get_or_insert(error);
                        Ordering::Equal
                    }
                },
            };
            let order = if descending { order.reverse() } else { order };
            if order != Ordering::Equal {
                return order;
            }
        }
        Ordering::Equal
    });
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(frame.take_rows(&rows))
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
