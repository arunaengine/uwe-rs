//! Operations on the values of a column: types, casts, mapping, sorting and element-wise
//! operators, with pandas' rules for missing values.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cmp::Ordering;

use super::{DataFrame, Series, is_list_like, is_missing, nan, same_label, seq_values};
use crate::error::Error;
use crate::interpreter::{Interpreter, Operator};
use crate::value::{Args, Value};

/// The name of the type pandas gives to a column of these values.
pub fn dtype_name(values: &[Value]) -> &'static str {
    let present: Vec<&Value> = values.iter().filter(|value| !is_missing(value)).collect();
    let missing = present.len() < values.len();
    if present.is_empty() {
        return if values.is_empty() {
            "object"
        } else {
            "float64"
        };
    }
    if present
        .iter()
        .all(|value| matches!(value.plain(), Value::Bool(_)))
    {
        return if missing { "object" } else { "bool" };
    }
    if !missing
        && present
            .iter()
            .all(|value| matches!(value.plain(), Value::Int(_)))
    {
        return "int64";
    }
    if present
        .iter()
        .all(|value| matches!(value.plain(), Value::Int(_) | Value::Float(_)))
    {
        return "float64";
    }
    if present
        .iter()
        .all(|value| matches!(value.plain(), Value::Str(_)))
    {
        return "str";
    }
    "object"
}

/// The values without repeats, in the order they first occur.
pub fn distinct(values: &[Value]) -> Vec<Value> {
    let mut result: Vec<Value> = Vec::new();
    for value in values {
        if !result.iter().any(|seen| same_label(seen, value)) {
            result.push(value.clone());
        }
    }
    result
}

/// `True` for each value that occurred before, as `duplicated(keep="first")` marks them.
pub fn duplicated(values: &[Value]) -> Vec<Value> {
    values
        .iter()
        .enumerate()
        .map(|(position, value)| {
            Value::Bool(
                values[..position]
                    .iter()
                    .any(|seen| same_label(seen, value)),
            )
        })
        .collect()
}

/// The type a column is cast to, or read as.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DtypeKind {
    Str,
    Object,
    Category,
    Int,
    NullableInt,
    Float,
    Bool,
}

/// The kind of a type given as a class like `str`, a name like `"int64"`, or a NumPy type.
pub fn dtype_kind(value: &Value) -> Option<DtypeKind> {
    let name: String = match value.plain() {
        Value::Class(class) => class.name.to_string(),
        Value::Str(text) => text.to_string(),
        Value::Builtin(builtin) => builtin.name.to_string(),
        _ => return None,
    };
    Some(match name.as_str() {
        "str" | "string" | "U" | "string[python]" | "string[pyarrow]" | "str_" => DtypeKind::Str,
        "object" | "O" => DtypeKind::Object,
        "category" => DtypeKind::Category,
        "int" | "int64" | "int32" | "int16" | "int8" | "uint64" | "uint32" | "uint16" | "uint8"
        | "i8" | "int_" => DtypeKind::Int,
        "Int64" | "Int32" | "Int16" | "Int8" | "UInt64" | "UInt32" => DtypeKind::NullableInt,
        "float" | "float64" | "float32" | "f8" | "Float64" | "float_" => DtypeKind::Float,
        "bool" | "boolean" | "bool_" => DtypeKind::Bool,
        _ => return None,
    })
}

/// The values cast to a type, as `astype` does.
pub fn cast_values(
    interpreter: &mut Interpreter,
    values: &[Value],
    kind: &Value,
) -> Result<Vec<Value>, Error> {
    let Some(target) = dtype_kind(kind) else {
        return Err(Error::unsupported(format!(
            "astype({})",
            crate::format::plain_repr(kind)
        )));
    };
    let mut result = Vec::with_capacity(values.len());
    for value in values {
        let missing = is_missing(value);
        result.push(match target {
            DtypeKind::Object | DtypeKind::Category => value.clone(),
            DtypeKind::Str if missing => nan(),
            DtypeKind::Str => match value.plain() {
                Value::Str(_) => value.plain().clone(),
                other => Value::str(interpreter.to_str(other)?),
            },
            DtypeKind::Int | DtypeKind::NullableInt if missing => {
                if target == DtypeKind::NullableInt {
                    nan()
                } else {
                    return Err(Error::new(
                        "IntCastingNaNError",
                        "Cannot convert non-finite values (NA or inf) to integer",
                    ));
                }
            }
            DtypeKind::Int | DtypeKind::NullableInt => match value.plain() {
                Value::Str(text) => crate::number::parse_int(text.trim(), 10)?,
                other => crate::builtins::to_int(interpreter, other)?,
            },
            DtypeKind::Float if missing => nan(),
            DtypeKind::Float => match value.plain() {
                Value::Str(text) => Value::Float(crate::number::parse_float(text)?),
                other => Value::Float(crate::number::to_f64(other).ok_or_else(|| {
                    Error::value_error(format!(
                        "could not convert {} to float",
                        crate::format::plain_repr(other)
                    ))
                })?),
            },
            DtypeKind::Bool => Value::Bool(missing || interpreter.truthy(value)?),
        });
    }
    Ok(result)
}

/// The values mapped through a function, a dict or a series, as `map` does.
pub fn map_values(
    interpreter: &mut Interpreter,
    values: &[Value],
    mapping: &Value,
) -> Result<Vec<Value>, Error> {
    let mut result = Vec::with_capacity(values.len());
    for value in values {
        result.push(match mapping.plain() {
            Value::Dict(dict) => match crate::value::Key::new(value.clone()) {
                Ok(key) => dict.borrow().entries.get(&key).cloned().unwrap_or_else(nan),
                Err(_) => nan(),
            },
            Value::Native(object) if object.as_any().is::<Series>() => {
                let series = mapping.as_native::<Series>().map(Series::data);
                series
                    .and_then(|series| {
                        series
                            .index
                            .find(value)
                            .first()
                            .map(|position| series.values[*position].clone())
                    })
                    .unwrap_or_else(nan)
            }
            _ => interpreter.call(mapping, Args::new(vec![value.clone()]))?,
        });
    }
    Ok(result)
}

/// `replace(to_replace, value)` with one value, a list of values or a dict.
pub fn replace_values(
    interpreter: &mut Interpreter,
    values: &[Value],
    args: &Args,
) -> Result<Vec<Value>, Error> {
    let to_replace = args
        .get(0)
        .cloned()
        .or_else(|| args.keyword("to_replace").cloned())
        .unwrap_or(Value::None);
    let replacement = args
        .get(1)
        .cloned()
        .or_else(|| args.keyword("value").cloned())
        .unwrap_or(Value::None);
    let pairs: Vec<(Value, Value)> = match to_replace.plain() {
        Value::Dict(_) => interpreter.mapping_items(&to_replace)?,
        _ if is_list_like(&to_replace) => {
            let old = seq_values(interpreter, &to_replace)?;
            if is_list_like(&replacement) {
                old.into_iter()
                    .zip(seq_values(interpreter, &replacement)?)
                    .collect()
            } else {
                old.into_iter()
                    .map(|old| (old, replacement.clone()))
                    .collect()
            }
        }
        _ => vec![(to_replace, replacement)],
    };
    Ok(values
        .iter()
        .map(|value| {
            pairs
                .iter()
                .find(|(old, _)| same_label(old, value))
                .map_or_else(|| value.clone(), |(_, new)| new.clone())
        })
        .collect())
}

/// The positions of the keys in sorted order; missing keys come last.
pub fn sorted_positions(
    interpreter: &mut Interpreter,
    keys: &[Value],
    reverse: bool,
) -> Result<Vec<usize>, Error> {
    let has_missing = |value: &Value| match value.plain() {
        Value::Tuple(parts) => parts.iter().any(is_missing),
        other => is_missing(other),
    };
    let present: Vec<Value> = keys
        .iter()
        .enumerate()
        .filter(|(_, key)| !has_missing(key))
        .map(|(position, key)| Value::tuple(vec![key.clone(), Value::Int(position as i64)]))
        .collect();
    let key = Value::builtin("first", |interpreter, args| {
        let item = args.get(0).cloned().unwrap_or(Value::None);
        interpreter.get_item(&item, &Value::Int(0))
    });
    let sorted = crate::builtins::sort_values(interpreter, present, Some(&key), reverse)?;
    let mut order: Vec<usize> = sorted
        .iter()
        .filter_map(|item| match item.plain() {
            Value::Tuple(parts) => parts
                .get(1)
                .and_then(Value::as_int)
                .map(|position| position as usize),
            _ => None,
        })
        .collect();
    order.extend((0..keys.len()).filter(|position| has_missing(&keys[*position])));
    Ok(order)
}

/// The sum of the values that are not missing.
pub fn sum(interpreter: &mut Interpreter, values: &[Value]) -> Result<Value, Error> {
    let mut total = Value::Int(0);
    for value in values.iter().filter(|value| !is_missing(value)) {
        total = interpreter.binary(Operator::Add, &total, value)?;
    }
    Ok(total)
}

/// The largest or smallest value that is not missing, `nan` for none.
pub fn extreme(
    interpreter: &mut Interpreter,
    values: &[Value],
    largest: bool,
) -> Result<Value, Error> {
    let mut best: Option<Value> = None;
    for value in values.iter().filter(|value| !is_missing(value)) {
        best = Some(match best {
            None => value.clone(),
            Some(current) => {
                let ordering = interpreter.compare(value, &current)?;
                if (largest && ordering == Ordering::Greater)
                    || (!largest && ordering == Ordering::Less)
                {
                    value.clone()
                } else {
                    current
                }
            }
        });
    }
    Ok(best.unwrap_or_else(nan))
}

/// An operator applied to each value and the matching value of `other`, or `other` itself
/// when it is one value. Missing values compare unequal and give missing results.
pub fn elementwise(
    interpreter: &mut Interpreter,
    operator: Operator,
    values: &[Value],
    other: &Value,
    reflected: bool,
) -> Result<Option<Vec<Value>>, Error> {
    if other.as_native::<DataFrame>().is_some() {
        return Ok(None);
    }
    let others: Option<Vec<Value>> = if is_list_like(other)
        && !matches!(
            other.plain(),
            Value::Dict(_) | Value::Set(_) | Value::FrozenSet(_)
        ) {
        let others = seq_values(interpreter, other)?;
        if others.len() != values.len() {
            return Err(Error::value_error("Lengths must match to compare"));
        }
        Some(others)
    } else {
        None
    };
    let mut result = Vec::with_capacity(values.len());
    for (position, value) in values.iter().enumerate() {
        let item = others.as_ref().map_or(other, |others| &others[position]);
        let (a, b) = if reflected {
            (item, value)
        } else {
            (value, item)
        };
        result.push(if is_missing(a) || is_missing(b) {
            match operator {
                Operator::NotEq => Value::Bool(true),
                Operator::Eq
                | Operator::Lt
                | Operator::LtE
                | Operator::Gt
                | Operator::GtE
                | Operator::BitAnd => Value::Bool(false),
                Operator::BitOr => Value::Bool(
                    !is_missing(a) && interpreter.truthy(a)?
                        || !is_missing(b) && interpreter.truthy(b)?,
                ),
                _ => nan(),
            }
        } else {
            interpreter.binary(operator, a, b)?
        });
    }
    Ok(Some(result))
}

/// `~`, `-` and `+` applied to each value.
pub fn unary_values(
    interpreter: &mut Interpreter,
    operator: &str,
    values: &[Value],
) -> Result<Option<Vec<Value>>, Error> {
    if !matches!(operator, "~" | "-" | "+") {
        return Ok(None);
    }
    let mut result = Vec::with_capacity(values.len());
    for value in values {
        result.push(match (operator, value.plain()) {
            (_, _) if is_missing(value) => nan(),
            ("~", Value::Bool(flag)) => Value::Bool(!flag),
            _ => interpreter.unary(operator, value)?,
        });
    }
    Ok(Some(result))
}

/// `pd.to_numeric(arg, errors="raise")` for one value, a sequence or a series.
pub fn to_numeric(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let value = args.take(0, "arg").unwrap_or(Value::None);
    let errors = args
        .take_keyword("errors")
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "raise".into());
    let convert = |value: &Value| -> Result<Value, Error> {
        if is_missing(value) {
            return Ok(nan());
        }
        let Value::Str(text) = value.plain() else {
            return Ok(value.clone());
        };
        let text = text.trim();
        if let Ok(number) = text.parse::<i64>() {
            return Ok(Value::Int(number));
        }
        if let Ok(number) = crate::number::parse_float(text) {
            return Ok(Value::Float(number));
        }
        match errors.as_str() {
            "coerce" => Ok(nan()),
            "ignore" => Ok(value.clone()),
            _ => Err(Error::value_error(format!(
                "Unable to parse string \"{text}\""
            ))),
        }
    };
    if let Some(series) = value.as_native::<Series>() {
        let data = series.data();
        let values = data
            .values
            .iter()
            .map(convert)
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(Series::from_data(data.with_values(values)));
    }
    if is_list_like(&value) {
        let values = seq_values(interpreter, &value)?;
        let values = values.iter().map(convert).collect::<Result<Vec<_>, _>>()?;
        return Ok(super::Array::value(values));
    }
    convert(&value)
}
