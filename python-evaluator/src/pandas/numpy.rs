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
        (
            "arange",
            function("arange", |_, args| {
                let numbers: Vec<i64> = args.positional.iter().filter_map(Value::as_int).collect();
                let (start, stop, step) = match numbers.as_slice() {
                    [stop] => (0, *stop, 1),
                    [start, stop] => (*start, *stop, 1),
                    [start, stop, step, ..] => (*start, *stop, *step),
                    [] => return Err(Error::unsupported("numpy.arange of other than integers")),
                };
                if step == 0 {
                    return Err(Error::value_error("Maximum allowed size exceeded"));
                }
                let mut values = Vec::new();
                let mut next = start;
                while (step > 0 && next < stop) || (step < 0 && next > stop) {
                    values.push(Value::Int(next));
                    next += step;
                }
                Ok(Array::value(values))
            }),
        ),
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
                super::sum(interpreter, &values)
            }),
        ),
        (
            "max",
            function("max", |interpreter, args| {
                let values = seq_values(interpreter, args.get(0).unwrap_or(&Value::None))?;
                super::extreme(interpreter, &values, true)
            }),
        ),
        (
            "min",
            function("min", |interpreter, args| {
                let values = seq_values(interpreter, args.get(0).unwrap_or(&Value::None))?;
                super::extreme(interpreter, &values, false)
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
