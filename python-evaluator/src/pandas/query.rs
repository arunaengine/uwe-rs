//! `DataFrame.query`: the rows for which an expression over the columns is true. As in
//! pandas, `and`, `or` and `not` work on each row, and `==` or `in` with a list tests
//! membership. Other expressions run in the interpreter with the columns as series.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use ruff_python_ast::{BoolOp, CmpOp, Expr, UnaryOp};

use super::frame::FrameData;
use super::series::Series;
use super::{is_list_like, same_label, seq_values};
use crate::error::Error;
use crate::interpreter::{Frame, Interpreter, Operator, Source};
use crate::value::{Scope, Value};

/// Replaces names in backticks by placeholders, which the scope then binds.
fn replace_backticks(text: &str) -> (String, Vec<(String, String)>) {
    let mut result = String::with_capacity(text.len());
    let mut names = Vec::new();
    let mut parts = text.split('`');
    if let Some(first) = parts.next() {
        result.push_str(first);
    }
    let rest: Vec<&str> = parts.collect();
    for (position, part) in rest.iter().enumerate() {
        if position % 2 == 0 {
            let placeholder = format!("__backtick_{}__", names.len());
            names.push((placeholder.clone(), (*part).to_owned()));
            result.push_str(&placeholder);
        } else {
            result.push_str(part);
        }
    }
    (result, names)
}

/// The rows of the table for which the query is true.
pub fn query(
    interpreter: &mut Interpreter,
    data: &FrameData,
    text: &str,
) -> Result<FrameData, Error> {
    let (text, quoted) = replace_backticks(text);
    if text.contains('@') {
        return Err(Error::unsupported(
            "DataFrame.query with local variables (@name)",
        ));
    }
    let parsed = ruff_python_parser::parse_expression(&text)
        .map_err(|error| Error::new("SyntaxError", error.to_string()))?;
    let expression = parsed.into_syntax().body;
    let scope = Scope::new(None);
    let index = data.index();
    scope.set(
        "index",
        Series::value(index.level(0), index.clone(), index.names[0].clone()),
    );
    for level in 0..index.levels() {
        if let Some(name) = index.names[level].as_str() {
            scope.set(
                name,
                Series::value(
                    index.level(level),
                    index.clone(),
                    index.names[level].clone(),
                ),
            );
        }
    }
    for (position, label) in data.columns.values.iter().enumerate() {
        if let Some(name) = label.as_str() {
            scope.set(name, Series::from_data(data.column(position)));
        }
    }
    for (placeholder, name) in &quoted {
        if let Some(position) = data.column_position(&Value::str(name.as_str())) {
            scope.set(placeholder, Series::from_data(data.column(position)));
        }
    }
    let frame = Frame::module(scope, Source::new("<query>", &text));
    let mask = evaluate(interpreter, &frame, &expression)?;
    let values = seq_values(interpreter, &mask)?;
    if values.len() != data.rows()
        || !values
            .iter()
            .all(|value| matches!(value.plain(), Value::Bool(_)))
    {
        return Err(Error::value_error(
            "the query must give one boolean for each row",
        ));
    }
    let rows: Vec<usize> = values
        .iter()
        .enumerate()
        .filter(|(_, value)| matches!(value.plain(), Value::Bool(true)))
        .map(|(row, _)| row)
        .collect();
    Ok(data.take_rows(&rows))
}

fn evaluate(
    interpreter: &mut Interpreter,
    frame: &Frame,
    expression: &Expr,
) -> Result<Value, Error> {
    match expression {
        Expr::BoolOp(operation) => {
            let operator = match operation.op {
                BoolOp::And => Operator::BitAnd,
                BoolOp::Or => Operator::BitOr,
            };
            let mut result = evaluate(interpreter, frame, &operation.values[0])?;
            for value in &operation.values[1..] {
                let value = evaluate(interpreter, frame, value)?;
                result = interpreter.binary(operator, &result, &value)?;
            }
            Ok(result)
        }
        Expr::UnaryOp(operation) if operation.op == UnaryOp::Not => {
            let operand = evaluate(interpreter, frame, &operation.operand)?;
            interpreter.unary("~", &operand)
        }
        Expr::Compare(compare) => {
            let mut left = evaluate(interpreter, frame, &compare.operands[0])?;
            let mut result: Option<Value> = None;
            for (operator, right) in compare.ops.iter().zip(compare.operands[1..].iter()) {
                let right = evaluate(interpreter, frame, right)?;
                let outcome = compare_values(interpreter, *operator, &left, &right)?;
                result = Some(match result {
                    Some(previous) => interpreter.binary(Operator::BitAnd, &previous, &outcome)?,
                    None => outcome,
                });
                left = right;
            }
            Ok(result.unwrap_or(Value::Bool(true)))
        }
        _ => interpreter.eval(frame, expression),
    }
}

fn compare_values(
    interpreter: &mut Interpreter,
    operator: CmpOp,
    left: &Value,
    right: &Value,
) -> Result<Value, Error> {
    let membership = matches!(operator, CmpOp::In | CmpOp::NotIn)
        || (matches!(operator, CmpOp::Eq | CmpOp::NotEq)
            && is_list_like(right)
            && right.as_native::<Series>().is_none());
    if membership {
        let wanted = seq_values(interpreter, right)?;
        let negate = matches!(operator, CmpOp::NotIn | CmpOp::NotEq);
        let test = |value: &Value| {
            Value::Bool(wanted.iter().any(|item| same_label(item, value)) != negate)
        };
        if let Some(series) = left.as_native::<Series>() {
            let data = series.data();
            return Ok(Series::from_data(
                data.with_values(data.values.iter().map(test).collect()),
            ));
        }
        return Ok(test(left));
    }
    let operator = match operator {
        CmpOp::Eq => Operator::Eq,
        CmpOp::NotEq => Operator::NotEq,
        CmpOp::Lt => Operator::Lt,
        CmpOp::LtE => Operator::LtE,
        CmpOp::Gt => Operator::Gt,
        CmpOp::GtE => Operator::GtE,
        CmpOp::Is | CmpOp::IsNot | CmpOp::In | CmpOp::NotIn => {
            return Err(Error::unsupported("is and is not in DataFrame.query"));
        }
    };
    interpreter.binary(operator, left, right)
}
