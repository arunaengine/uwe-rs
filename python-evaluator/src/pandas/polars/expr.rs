//! Evaluates polars column and literal expressions into table columns.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::rc::Rc;

use super::super::frame::FrameData;
use super::dtype::{DType, cast, of_value};
use crate::error::Error;
use crate::interpreter::{Interpreter, Operator};
use crate::value::{Native, Value, key_equal};

pub enum Kind {
    Column(String),
    Literal(Value),
    Alias(Rc<Kind>, String),
    Compare(Operator, Rc<Kind>, Rc<Kind>),
    IsIn(Rc<Kind>, Vec<Value>),
    IsNull(Rc<Kind>, bool),
    Cast(Rc<Kind>, DType),
    And(Rc<Kind>, Rc<Kind>),
    Or(Rc<Kind>, Rc<Kind>),
    Not(Rc<Kind>),
}

/// A polars expression.
pub struct Expr(pub Rc<Kind>);

impl Expr {
    pub fn value(kind: Kind) -> Value {
        Expr(Rc::new(kind)).into_value()
    }

    pub fn into_value(self) -> Value {
        Value::native(self)
    }

    /// An expression for a value: itself, or a literal.
    pub fn of(value: &Value) -> Rc<Kind> {
        match value.as_native::<Expr>() {
            Some(expr) => expr.0.clone(),
            None => Rc::new(Kind::Literal(value.clone())),
        }
    }
}

/// The name of the column an expression gives: its alias, else its first column.
pub fn output_name(kind: &Kind) -> Option<String> {
    match kind {
        Kind::Column(name) | Kind::Alias(_, name) => Some(name.clone()),
        Kind::Literal(_) => Some("literal".to_owned()),
        Kind::Compare(_, a, _) | Kind::And(a, _) | Kind::Or(a, _) => output_name(a),
        Kind::IsIn(a, _) | Kind::IsNull(a, _) | Kind::Cast(a, _) | Kind::Not(a) => output_name(a),
    }
}

/// The truth value of a cell, or `None` for null.
fn truth(value: &Value) -> Option<bool> {
    match value.plain() {
        Value::None => None,
        Value::Bool(flag) => Some(*flag),
        Value::Int(number) => Some(*number != 0),
        _ => Some(true),
    }
}

/// The values of an expression for each row of a table.
pub fn evaluate(
    interpreter: &mut Interpreter,
    kind: &Kind,
    data: &FrameData,
) -> Result<Vec<Value>, Error> {
    let rows = data.rows();
    Ok(match kind {
        Kind::Column(name) => {
            let position = data
                .column_position(&Value::str(name.as_str()))
                .ok_or_else(|| {
                    super::error(
                        "ColumnNotFoundError",
                        format!("unable to find column \"{name}\""),
                    )
                })?;
            data.data[position].clone()
        }
        Kind::Literal(value) => vec![value.clone(); rows],
        Kind::Alias(inner, _) => evaluate(interpreter, inner, data)?,
        Kind::Compare(operator, a, b) => {
            let (a, b) = (
                evaluate(interpreter, a, data)?,
                evaluate(interpreter, b, data)?,
            );
            let mut result = Vec::with_capacity(rows);
            for (x, y) in a.iter().zip(b.iter()) {
                result.push(if x.is_none() || y.is_none() {
                    Value::None
                } else {
                    interpreter.binary(*operator, x, y)?
                });
            }
            result
        }
        Kind::IsIn(a, items) => evaluate(interpreter, a, data)?
            .iter()
            .map(|value| match value {
                Value::None => Value::None,
                _ => Value::Bool(items.iter().any(|item| key_equal(item, value))),
            })
            .collect(),
        Kind::IsNull(a, null) => evaluate(interpreter, a, data)?
            .iter()
            .map(|value| Value::Bool(value.is_none() == *null))
            .collect(),
        Kind::Cast(a, dtype) => {
            let values = evaluate(interpreter, a, data)?;
            let mut result = Vec::with_capacity(values.len());
            for value in &values {
                result.push(cast(interpreter, value, *dtype)?);
            }
            result
        }
        Kind::And(a, b) | Kind::Or(a, b) => {
            let (a, b) = (
                evaluate(interpreter, a, data)?,
                evaluate(interpreter, b, data)?,
            );
            // Null is an unknown truth value: `false & null` is false, `true | null` is true.
            let decisive = !matches!(kind, Kind::And(..));
            a.iter()
                .zip(b.iter())
                .map(|(x, y)| match (truth(x), truth(y)) {
                    (Some(x), _) if x == decisive => Value::Bool(decisive),
                    (_, Some(y)) if y == decisive => Value::Bool(decisive),
                    (Some(_), Some(_)) => Value::Bool(!decisive),
                    _ => Value::None,
                })
                .collect()
        }
        Kind::Not(a) => evaluate(interpreter, a, data)?
            .iter()
            .map(|value| truth(value).map_or(Value::None, |flag| Value::Bool(!flag)))
            .collect(),
    })
}

fn compare_method(this: &Rc<Kind>, name: &str, operator: Operator) -> Value {
    let this = this.clone();
    Value::builtin(name, move |_, args| {
        let other = Expr::of(args.get(0).unwrap_or(&Value::None));
        Ok(Expr::value(Kind::Compare(operator, this.clone(), other)))
    })
}

impl Native for Expr {
    fn type_name(&self) -> &str {
        "Expr"
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
        let this = self.0.clone();
        Ok(Some(match name {
            "alias" => Value::builtin("alias", move |interpreter, args| {
                let alias = interpreter.to_str(args.get(0).unwrap_or(&Value::None))?;
                Ok(Expr::value(Kind::Alias(this.clone(), alias)))
            }),
            "eq" => compare_method(&this, name, Operator::Eq),
            "ne" => compare_method(&this, name, Operator::NotEq),
            "lt" => compare_method(&this, name, Operator::Lt),
            "le" => compare_method(&this, name, Operator::LtE),
            "gt" => compare_method(&this, name, Operator::Gt),
            "ge" => compare_method(&this, name, Operator::GtE),
            "is_in" => Value::builtin("is_in", move |interpreter, args| {
                let items = interpreter.iterate(args.get(0).unwrap_or(&Value::None))?;
                Ok(Expr::value(Kind::IsIn(this.clone(), items)))
            }),
            "is_null" | "is_not_null" => {
                let null = name == "is_null";
                Value::builtin(name, move |_, _| {
                    Ok(Expr::value(Kind::IsNull(this.clone(), null)))
                })
            }
            "cast" => Value::builtin("cast", move |_, args| {
                let dtype = of_value(args.get(0).unwrap_or(&Value::None))?;
                Ok(Expr::value(Kind::Cast(this.clone(), dtype)))
            }),
            _ => return Ok(None),
        }))
    }

    fn binary(
        &self,
        _: &mut Interpreter,
        operator: Operator,
        other: &Value,
        reflected: bool,
    ) -> Result<Option<Value>, Error> {
        let (a, b) = if reflected {
            (Expr::of(other), self.0.clone())
        } else {
            (self.0.clone(), Expr::of(other))
        };
        Ok(Some(match operator {
            Operator::BitAnd => Expr::value(Kind::And(a, b)),
            Operator::BitOr => Expr::value(Kind::Or(a, b)),
            Operator::Eq
            | Operator::NotEq
            | Operator::Lt
            | Operator::LtE
            | Operator::Gt
            | Operator::GtE => Expr::value(Kind::Compare(operator, a, b)),
            _ => return Ok(None),
        }))
    }

    fn unary(&self, _: &mut Interpreter, operator: &str) -> Result<Option<Value>, Error> {
        Ok(match operator {
            "~" => Some(Expr::value(Kind::Not(self.0.clone()))),
            _ => None,
        })
    }
}
