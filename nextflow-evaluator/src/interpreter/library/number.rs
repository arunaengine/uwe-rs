//! Methods of numbers, `MemoryUnit` and `Duration`, and the unit properties like `2.GB`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_parser::Span;

use super::super::{AtSpan, Error, Interpreter, thrown};
use crate::exception::Exception;
use crate::number::{BigDecimal, Number};
use crate::units::{
    check_count, duration_unit, is_memory_unit, java_round, parse_duration, parse_memory,
};
use crate::value::{Operator, Value};

impl Interpreter<'_> {
    pub(super) fn number_method(
        &mut self,
        number: &Number,
        name: &str,
        arguments: &[Value],
        span: Span,
    ) -> Result<Option<Value>, Error> {
        let object = Value::Number(number.clone());
        Ok(Some(match (name, arguments) {
            ("intValue" | "toInteger", []) => Value::integer(number.to_i32()),
            ("longValue" | "toLong", []) => Value::Number(Number::Long(number.to_i64())),
            ("doubleValue" | "toDouble", []) => Value::Number(Number::Double(number.to_f64())),
            ("floatValue" | "toFloat", []) => Value::Number(Number::Float(number.to_f64() as f32)),
            ("toBigDecimal", []) => Value::Number(Number::BigDecimal(number.to_big_decimal())),
            ("toBigInteger", []) => Value::Number(Number::BigInteger(number.to_bigint())),
            ("round", []) => Value::Number(match number {
                Number::BigDecimal(value) => Number::BigDecimal(value.set_scale_half_up(0)),
                Number::Double(value) => Number::Long(java_round(*value)),
                Number::Float(value) => Number::Integer(java_round(f64::from(*value)) as i32),
                other => other.clone(),
            }),
            ("round", [Value::Number(places)]) => {
                let places = places.to_i32();
                let decimal = number.to_big_decimal().set_scale_half_up(places);
                Value::Number(match number {
                    Number::Double(_) => Number::Double(decimal.to_f64()),
                    Number::Float(_) => Number::Float(decimal.to_f64() as f32),
                    _ => Number::BigDecimal(decimal),
                })
            }
            ("abs", []) => {
                if number.compare(&Number::Integer(0)).is_lt() {
                    Value::Number(number.negate())
                } else {
                    object
                }
            }
            ("intdiv", [Value::Number(other)]) => {
                Value::Number(number.intdiv(other).map_err(Exception::from).at(span)?)
            }
            ("isNaN", []) => Value::Boolean(number.to_f64().is_nan()),
            ("isInfinite", []) => Value::Boolean(number.to_f64().is_infinite()),
            ("compareTo", [other]) => object.spaceship(other).at(span)?,
            ("times", [Value::Closure(f)]) => {
                for index in 0..number.to_i64().max(0) {
                    let index = Value::Number(integer_like(number, index));
                    self.call_closure_value(f, vec![index], span)?;
                }
                Value::Null
            }
            ("plus" | "minus" | "multiply" | "div" | "mod" | "power", [other]) => {
                let operator = match name {
                    "plus" => Operator::Add,
                    "minus" => Operator::Subtract,
                    "multiply" => Operator::Multiply,
                    "div" => Operator::Divide,
                    "mod" => Operator::Modulo,
                    _ => Operator::Power,
                };
                object.apply(operator, other).at(span)?
            }
            _ => return Ok(None),
        }))
    }
}

/// Methods of `MemoryUnit` (bytes) and `Duration` (milliseconds).
pub(super) fn unit_method(value: &Value, name: &str, arguments: &[Value]) -> Option<Value> {
    let long = |count: i64| Some(Value::Number(Number::Long(count)));
    match (value, name, arguments) {
        (Value::MemoryUnit(size), "toBytes" | "getBytes", []) => long(*size),
        (Value::MemoryUnit(size), "toKilo" | "getKilo", []) => long(size >> 10),
        (Value::MemoryUnit(size), "toMega" | "getMega", []) => long(size >> 20),
        (Value::MemoryUnit(size), "toGiga" | "getGiga", []) => long(size >> 30),
        (Value::MemoryUnit(size), "toUnit", [Value::String(unit)]) => {
            let units = ["B", "KB", "MB", "GB", "TB", "PB", "EB", "ZB"];
            let index = units.iter().position(|name| **name == **unit)?;
            long(size / 1024i64.pow(index as u32))
        }
        (Value::Duration(millis), "toMillis" | "getMillis", []) => long(*millis),
        (Value::Duration(millis), "toSeconds" | "getSeconds", []) => long(millis / 1_000),
        (Value::Duration(millis), "toMinutes" | "getMinutes", []) => long(millis / 60_000),
        (Value::Duration(millis), "toHours" | "getHours", []) => long(millis / 3_600_000),
        (Value::Duration(millis), "toDays" | "getDays", []) => long(millis / 86_400_000),
        // Legacy configs compare units, like `obj.compareTo(params.max_memory as MemoryUnit)`.
        (Value::MemoryUnit(a), "compareTo", [Value::MemoryUnit(b)])
        | (Value::Duration(a), "compareTo", [Value::Duration(b)]) => {
            Some(Value::integer(a.cmp(b) as i32))
        }
        _ => None,
    }
}

/// `n.GB`, `n.h` and the other unit properties of a number: the number's text plus the unit,
/// parsed as a `Duration` for a lower-case duration unit, else as a `MemoryUnit`.
pub(in crate::interpreter) fn unit_property(
    number: &Number,
    name: &str,
    span: Span,
) -> Result<Value, Error> {
    let lower = name.to_lowercase();
    let upper = name.to_uppercase();
    if duration_unit(&lower).is_some() {
        return Ok(Value::Duration(
            parse_duration(&format!("{number}{lower}")).at(span)?,
        ));
    }
    if is_memory_unit(&upper) {
        return Ok(Value::MemoryUnit(
            parse_memory(&format!("{number}{upper}")).at(span)?,
        ));
    }
    let message = format!(
        "No such property: {name} for class: {}",
        number.java_class()
    );
    Err(thrown(
        Exception::new("MissingPropertyException", message),
        span,
    ))
}

/// A counter of the same integer class as the receiver of `times`.
fn integer_like(number: &Number, index: i64) -> Number {
    match number {
        Number::Long(_) => Number::Long(index),
        Number::BigInteger(_) => Number::BigInteger(index.into()),
        _ => Number::Integer(index as i32),
    }
}

/// `as` conversions to and from numbers and units.
pub(in crate::interpreter) fn convert(
    value: &Value,
    class: &str,
) -> Option<Result<Value, Exception>> {
    let text = match value {
        Value::String(text) | Value::GString(text) => Some(text.trim_matches(|c: char| c <= ' ')),
        _ => None,
    };
    Some(match (class, value, text) {
        ("Double", Value::Number(number), _) => Ok(Value::Number(Number::Double(number.to_f64()))),
        ("Float", Value::Number(number), _) => {
            Ok(Value::Number(Number::Float(number.to_f64() as f32)))
        }
        ("BigDecimal", Value::Number(number), _) => {
            Ok(Value::Number(Number::BigDecimal(number.to_big_decimal())))
        }
        ("BigInteger", Value::Number(number), _) => {
            Ok(Value::Number(Number::BigInteger(number.to_bigint())))
        }
        ("Double" | "Float", _, Some(text)) => match text.parse::<f64>() {
            Ok(parsed) if class == "Double" => Ok(Value::Number(Number::Double(parsed))),
            Ok(parsed) => Ok(Value::Number(Number::Float(parsed as f32))),
            Err(_) => Err(Exception::number_format(text)),
        },
        ("BigDecimal", _, Some(text)) => BigDecimal::parse(text)
            .map(|parsed| Value::Number(Number::BigDecimal(parsed)))
            .ok_or_else(|| Exception::number_format(text)),
        ("MemoryUnit", Value::Number(number), _) => {
            check_count(number.to_i64(), "Memory").map(Value::MemoryUnit)
        }
        ("Duration", Value::Number(number), _) => {
            check_count(number.to_i64(), "Duration").map(Value::Duration)
        }
        ("MemoryUnit", _, Some(text)) => parse_memory(text).map(Value::MemoryUnit),
        ("Duration", _, Some(text)) => parse_duration(text).map(Value::Duration),
        _ => return None,
    })
}
