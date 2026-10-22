//! A stand-in for the part of NumPy that workflows use while they are planned: `nan`, the
//! scalar types, and arrays made from lists.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;

use super::array::Array;
use super::{is_missing, seq_values};
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::modules::function;
use crate::types::TypeTag;
use crate::value::{Args, Native, Value};

/// `numpy.ndarray`, for `isinstance`.
struct ArrayClass;

impl Native for ArrayClass {
    fn type_name(&self) -> &str {
        "type"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn check_instance(&self, value: &Value) -> Option<bool> {
        Some(value.as_native::<Array>().is_some())
    }
}

/// A NumPy scalar type as the Python type it stands for, since the stand-in keeps numbers as
/// Python numbers: calls convert, and `isinstance` checks, as with the Python type.
fn scalar_type(name: &'static str, python: TypeTag) -> (&'static str, Value) {
    (name, Value::Class(crate::types::builtin_class(python)))
}

/// A function of one number, applied to each value of a sequence.
fn numeric(name: &'static str, apply: fn(f64) -> Value) -> (&'static str, Value) {
    (
        name,
        Value::builtin(name, move |interpreter, args| {
            let value = args.get(0).cloned().unwrap_or(Value::None);
            let one = |value: &Value| -> Result<Value, Error> {
                let number = crate::number::to_f64(value).ok_or_else(|| {
                    Error::type_error(format!("ufunc '{name}' not supported for the input types"))
                })?;
                Ok(apply(number))
            };
            if super::is_list_like(&value) {
                let values = seq_values(interpreter, &value)?;
                return Ok(Array::value(
                    values.iter().map(one).collect::<Result<Vec<_>, _>>()?,
                ));
            }
            one(&value)
        }),
    )
}

pub fn module() -> Vec<(&'static str, Value)> {
    vec![
        ("nan", super::nan()),
        ("NaN", super::nan()),
        ("inf", Value::Float(f64::INFINITY)),
        ("Inf", Value::Float(f64::INFINITY)),
        ("pi", Value::Float(std::f64::consts::PI)),
        ("e", Value::Float(std::f64::consts::E)),
        ("ndarray", Value::native(ArrayClass)),
        scalar_type("int64", TypeTag::Int),
        scalar_type("int32", TypeTag::Int),
        scalar_type("int_", TypeTag::Int),
        scalar_type("integer", TypeTag::Int),
        scalar_type("float64", TypeTag::Float),
        scalar_type("float32", TypeTag::Float),
        scalar_type("float_", TypeTag::Float),
        scalar_type("floating", TypeTag::Float),
        scalar_type("str_", TypeTag::Str),
        scalar_type("bool_", TypeTag::Bool),
        scalar_type("object_", TypeTag::Object),
        numeric("isnan", |number| Value::Bool(number.is_nan())),
        numeric("isinf", |number| Value::Bool(number.is_infinite())),
        numeric("isfinite", |number| Value::Bool(number.is_finite())),
        numeric("floor", |number| Value::Float(number.floor())),
        numeric("ceil", |number| Value::Float(number.ceil())),
        numeric("sqrt", |number| Value::Float(number.sqrt())),
        numeric("log", |number| Value::Float(number.ln())),
        numeric("log2", |number| Value::Float(number.log2())),
        numeric("log10", |number| Value::Float(number.log10())),
        numeric("exp", |number| Value::Float(number.exp())),
        numeric("abs", |number| Value::Float(number.abs())),
        (
            "array",
            function("array", |interpreter, args| {
                let values = seq_values(interpreter, args.get(0).unwrap_or(&Value::None))?;
                Ok(Array::value(values))
            }),
        ),
        (
            "asarray",
            function("asarray", |interpreter, args| {
                let values = seq_values(interpreter, args.get(0).unwrap_or(&Value::None))?;
                Ok(Array::value(values))
            }),
        ),
        (
            "unique",
            function("unique", |interpreter, args| {
                let values = seq_values(interpreter, args.get(0).unwrap_or(&Value::None))?;
                let distinct = super::distinct(&values);
                let sorted = crate::builtins::sort_values(interpreter, distinct, None, false)?;
                Ok(Array::value(sorted))
            }),
        ),
        ("arange", function("arange", arange)),
        (
            "concatenate",
            function("concatenate", |interpreter, args| {
                let mut values = Vec::new();
                for part in interpreter.iterate(args.get(0).unwrap_or(&Value::None))? {
                    values.extend(seq_values(interpreter, &part)?);
                }
                Ok(Array::value(values))
            }),
        ),
        (
            "zeros",
            function("zeros", |_, args| {
                let count = args.get(0).and_then(Value::as_int).unwrap_or(0).max(0) as usize;
                Ok(Array::value(vec![Value::Float(0.0); count]))
            }),
        ),
        (
            "ones",
            function("ones", |_, args| {
                let count = args.get(0).and_then(Value::as_int).unwrap_or(0).max(0) as usize;
                Ok(Array::value(vec![Value::Float(1.0); count]))
            }),
        ),
        (
            "sum",
            function("sum", |interpreter, args| {
                let values = seq_values(interpreter, args.get(0).unwrap_or(&Value::None))?;
                reduce(interpreter, &values, "sum")
            }),
        ),
        (
            "max",
            function("max", |interpreter, args| {
                let values = seq_values(interpreter, args.get(0).unwrap_or(&Value::None))?;
                reduce(interpreter, &values, "max")
            }),
        ),
        (
            "min",
            function("min", |interpreter, args| {
                let values = seq_values(interpreter, args.get(0).unwrap_or(&Value::None))?;
                reduce(interpreter, &values, "min")
            }),
        ),
        ("mean", function("mean", mean)),
        (
            "round",
            function("round", |interpreter, args| {
                let builtin = interpreter
                    .builtins
                    .get("round")
                    .ok_or_else(|| Error::name_error("round"))?;
                interpreter.call(&builtin, args)
            }),
        ),
    ]
}

fn mean(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let values = seq_values(interpreter, args.get(0).unwrap_or(&Value::None))?;
    if values.is_empty() || values.iter().any(is_missing) {
        return Ok(super::nan());
    }
    let total = super::sum(interpreter, &values)?;
    let total = crate::number::to_f64(&total).unwrap_or(f64::NAN);
    Ok(Value::Float(total / values.len() as f64))
}

fn arange(_: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    args.expect("arange", 0, 4)?;
    for (position, name) in ["start", "stop", "step", "dtype"].iter().enumerate() {
        if position < args.positional.len() && args.keyword(name).is_some() {
            return Err(Error::type_error(format!(
                "arange() got multiple values for '{name}'"
            )));
        }
    }
    let start = args.take(0, "start");
    let stop = args.take(1, "stop");
    let step = args.take(2, "step").unwrap_or(Value::Int(1));
    if args.take(3, "dtype").is_some_and(|value| !value.is_none()) || !args.keywords.is_empty() {
        return Err(Error::unsupported(
            "numpy.arange dtype or additional options",
        ));
    }
    let (start, stop) = match (start, stop) {
        (Some(start), Some(stop)) => (start, stop),
        (Some(stop), None) | (None, Some(stop)) => (Value::Int(0), stop),
        (None, None) => return Err(Error::type_error("arange() requires a stop argument")),
    };
    let mut values = Vec::new();
    if let (Some(start), Some(stop), Some(step)) = (start.as_int(), stop.as_int(), step.as_int()) {
        if step == 0 {
            return Err(Error::new("ZeroDivisionError", "division by zero"));
        }
        let (start, stop, step) = (i128::from(start), i128::from(stop), i128::from(step));
        let distance = if step > 0 { stop - start } else { start - stop };
        let count = (distance.max(0) + step.abs() - 1) / step.abs();
        let count = usize::try_from(count)
            .map_err(|_| Error::value_error("Maximum allowed size exceeded"))?;
        values
            .try_reserve(count)
            .map_err(|_| Error::value_error("Maximum allowed size exceeded"))?;
        values.extend((0..count).map(|index| Value::Int((start + index as i128 * step) as i64)));
    } else {
        let number = |value: &Value| {
            crate::number::to_f64(value)
                .ok_or_else(|| Error::type_error("arange requires real numbers"))
        };
        let (start, stop, step) = (number(&start)?, number(&stop)?, number(&step)?);
        if step == 0.0 {
            return Err(Error::new("ZeroDivisionError", "division by zero"));
        }
        if !start.is_finite() || !stop.is_finite() || !step.is_finite() {
            return Err(Error::value_error("arange requires finite bounds and step"));
        }
        let count = ((stop - start) / step).ceil().max(0.0);
        if !count.is_finite() || count >= usize::MAX as f64 {
            return Err(Error::value_error("Maximum allowed size exceeded"));
        }
        let count = count as usize;
        values
            .try_reserve(count)
            .map_err(|_| Error::value_error("Maximum allowed size exceeded"))?;
        let stride = (start + step) - start;
        values.extend((0..count).map(|index| Value::Float(start + index as f64 * stride)));
    }
    Ok(Array::value(values))
}

pub(super) fn reduce(
    interpreter: &mut Interpreter,
    values: &[Value],
    operation: &str,
) -> Result<Value, Error> {
    if values
        .iter()
        .any(|value| matches!(value.plain(), Value::Float(number) if number.is_nan()))
    {
        return Ok(super::nan());
    }
    if operation == "sum" {
        let mut total = Value::Int(0);
        for value in values {
            total = interpreter.binary(crate::interpreter::Operator::Add, &total, value)?;
        }
        return Ok(total);
    }
    if values.is_empty() {
        return Err(Error::value_error(
            "zero-size array to reduction operation which has no identity",
        ));
    }
    if values.iter().any(Value::is_none) {
        return Err(Error::type_error(
            "cannot compare None in a NumPy reduction",
        ));
    }
    super::extreme(interpreter, values, operation == "max")
}
