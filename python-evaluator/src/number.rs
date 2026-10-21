//! Arithmetic with Python's rules: integers that never overflow, floor division and modulo
//! that round toward negative infinity, and true division that gives floats.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cmp::Ordering;

use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive, Zero};

use crate::error::Error;
use crate::value::Value;

/// A number as arithmetic sees it.
enum Number {
    Int(BigInt),
    Float(f64),
}

fn number(value: &Value) -> Option<Number> {
    match value.plain() {
        Value::Bool(value) => Some(Number::Int(BigInt::from(i64::from(*value)))),
        Value::Int(value) => Some(Number::Int(BigInt::from(*value))),
        Value::BigInt(value) => Some(Number::Int((**value).clone())),
        Value::Float(value) => Some(Number::Float(*value)),
        _ => None,
    }
}

pub fn is_number(value: &Value) -> bool {
    matches!(
        value.plain(),
        Value::Bool(_) | Value::Int(_) | Value::BigInt(_) | Value::Float(_)
    )
}

pub fn to_f64(value: &Value) -> Option<f64> {
    match value.plain() {
        Value::Bool(value) => Some(f64::from(u8::from(*value))),
        Value::Int(value) => Some(*value as f64),
        Value::BigInt(value) => value.to_f64(),
        Value::Float(value) => Some(*value),
        _ => None,
    }
}

pub fn to_bigint(value: &Value) -> Option<BigInt> {
    match value.plain() {
        Value::Bool(value) => Some(BigInt::from(i64::from(*value))),
        Value::Int(value) => Some(BigInt::from(*value)),
        Value::BigInt(value) => Some((**value).clone()),
        _ => None,
    }
}

pub fn numbers_equal(a: &Value, b: &Value) -> bool {
    compare_numbers(a, b) == Some(Ordering::Equal)
}

pub fn compare_numbers(a: &Value, b: &Value) -> Option<Ordering> {
    match (a.plain(), b.plain()) {
        (Value::Int(a), Value::Int(b)) => Some(a.cmp(b)),
        (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
        _ => match (number(a)?, number(b)?) {
            (Number::Int(a), Number::Int(b)) => Some(a.cmp(&b)),
            (Number::Float(a), Number::Float(b)) => a.partial_cmp(&b),
            (Number::Int(a), Number::Float(b)) => compare_int_float(&a, b),
            (Number::Float(a), Number::Int(b)) => compare_int_float(&b, a).map(Ordering::reverse),
        },
    }
}

fn compare_int_float(a: &BigInt, b: f64) -> Option<Ordering> {
    if b.is_nan() {
        return None;
    }
    if b.is_infinite() {
        return Some(if b > 0.0 {
            Ordering::Less
        } else {
            Ordering::Greater
        });
    }
    let floor = b.floor();
    let whole = BigInt::from(floor as i128);
    match a.cmp(&whole) {
        Ordering::Equal if b > floor => Some(Ordering::Less),
        other => Some(other),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arithmetic {
    Add,
    Sub,
    Mul,
    TrueDiv,
    FloorDiv,
    Mod,
    Pow,
    LShift,
    RShift,
    And,
    Or,
    Xor,
}

/// Applies an arithmetic operator to two numbers; `None` when either is not a number.
pub fn arithmetic(op: Arithmetic, a: &Value, b: &Value) -> Result<Option<Value>, Error> {
    // The common case of small integers first.
    if let (Value::Int(x), Value::Int(y)) = (a.plain(), b.plain()) {
        let (x, y) = (*x, *y);
        let fast = match op {
            Arithmetic::Add => x.checked_add(y),
            Arithmetic::Sub => x.checked_sub(y),
            Arithmetic::Mul => x.checked_mul(y),
            Arithmetic::And => Some(x & y),
            Arithmetic::Or => Some(x | y),
            Arithmetic::Xor => Some(x ^ y),
            Arithmetic::FloorDiv if y != 0 && !(x == i64::MIN && y == -1) => {
                Some(floor_div_i64(x, y))
            }
            Arithmetic::Mod if y != 0 && !(x == i64::MIN && y == -1) => {
                Some(x - y * floor_div_i64(x, y))
            }
            _ => None,
        };
        if let Some(result) = fast {
            return Ok(Some(Value::Int(result)));
        }
    }
    let (Some(x), Some(y)) = (number(a), number(b)) else {
        return Ok(None);
    };
    let result = match (x, y) {
        (Number::Int(x), Number::Int(y)) => integer_op(op, x, y)?,
        (x, y) => {
            if matches!(
                op,
                Arithmetic::LShift
                    | Arithmetic::RShift
                    | Arithmetic::And
                    | Arithmetic::Or
                    | Arithmetic::Xor
            ) {
                return Ok(None);
            }
            let x = match x {
                Number::Int(x) => big_to_f64(&x)?,
                Number::Float(x) => x,
            };
            let y = match y {
                Number::Int(y) => big_to_f64(&y)?,
                Number::Float(y) => y,
            };
            Value::Float(float_op(op, x, y)?)
        }
    };
    Ok(Some(result))
}

fn big_to_f64(value: &BigInt) -> Result<f64, Error> {
    value
        .to_f64()
        .filter(|value| value.is_finite())
        .ok_or_else(|| Error::new("OverflowError", "int too large to convert to float"))
}

fn floor_div_i64(x: i64, y: i64) -> i64 {
    let quotient = x / y;
    if x % y != 0 && ((x < 0) != (y < 0)) {
        quotient - 1
    } else {
        quotient
    }
}

fn integer_op(op: Arithmetic, x: BigInt, y: BigInt) -> Result<Value, Error> {
    let result = match op {
        Arithmetic::Add => x + y,
        Arithmetic::Sub => x - y,
        Arithmetic::Mul => x * y,
        Arithmetic::And => x & y,
        Arithmetic::Or => x | y,
        Arithmetic::Xor => x ^ y,
        Arithmetic::TrueDiv => {
            if y.is_zero() {
                return Err(Error::zero_division("division by zero"));
            }
            return Ok(Value::Float(big_to_f64(&x)? / big_to_f64(&y)?));
        }
        Arithmetic::FloorDiv | Arithmetic::Mod => {
            if y.is_zero() {
                let message = if op == Arithmetic::Mod {
                    "integer modulo by zero"
                } else {
                    "integer division or modulo by zero"
                };
                return Err(Error::zero_division(message));
            }
            let quotient = floor_div_big(&x, &y);
            if op == Arithmetic::FloorDiv {
                quotient
            } else {
                x - y * quotient
            }
        }
        Arithmetic::Pow => {
            if y.is_negative() {
                let base = big_to_f64(&x)?;
                if base == 0.0 {
                    return Err(Error::zero_division(
                        "0.0 cannot be raised to a negative power",
                    ));
                }
                return Ok(Value::Float(base.powf(big_to_f64(&y)?)));
            }
            let exponent = y
                .to_u32()
                .ok_or_else(|| Error::new("OverflowError", "exponent too large"))?;
            num_traits::Pow::pow(x, exponent)
        }
        Arithmetic::LShift | Arithmetic::RShift => {
            if y.is_negative() {
                return Err(Error::value_error("negative shift count"));
            }
            let shift = y
                .to_usize()
                .ok_or_else(|| Error::new("OverflowError", "shift count too large"))?;
            if op == Arithmetic::LShift {
                x << shift
            } else {
                x >> shift
            }
        }
    };
    Ok(Value::from_bigint(result))
}

fn floor_div_big(x: &BigInt, y: &BigInt) -> BigInt {
    let quotient = x / y;
    let remainder = x % y;
    if !remainder.is_zero() && (x.is_negative() != y.is_negative()) {
        quotient - 1
    } else {
        quotient
    }
}

fn float_op(op: Arithmetic, x: f64, y: f64) -> Result<f64, Error> {
    Ok(match op {
        Arithmetic::Add => x + y,
        Arithmetic::Sub => x - y,
        Arithmetic::Mul => x * y,
        Arithmetic::TrueDiv => {
            if y == 0.0 {
                return Err(Error::zero_division("float division by zero"));
            }
            x / y
        }
        Arithmetic::FloorDiv => {
            if y == 0.0 {
                return Err(Error::zero_division("float floor division by zero"));
            }
            float_divmod(x, y).0
        }
        Arithmetic::Mod => {
            if y == 0.0 {
                return Err(Error::zero_division("float modulo"));
            }
            float_divmod(x, y).1
        }
        Arithmetic::Pow => {
            if x == 0.0 && y < 0.0 {
                return Err(Error::zero_division(
                    "0.0 cannot be raised to a negative power",
                ));
            }
            x.powf(y)
        }
        _ => unreachable!("bit operators take integers"),
    })
}

/// Python's `divmod` for floats.
pub fn float_divmod(x: f64, y: f64) -> (f64, f64) {
    let mut modulo = x % y;
    let mut quotient = (x - modulo) / y;
    if modulo != 0.0 {
        if (y < 0.0) != (modulo < 0.0) {
            modulo += y;
            quotient -= 1.0;
        }
    } else {
        modulo = 0.0f64.copysign(y);
    }
    let floor = if quotient != 0.0 {
        let mut floor = quotient.floor();
        if quotient - floor > 0.5 {
            floor += 1.0;
        }
        floor
    } else {
        0.0f64.copysign(x / y)
    };
    (floor, modulo)
}

/// Python's `repr()` of a float: the shortest text that reads back as the same number.
pub fn float_repr(value: f64) -> String {
    if value.is_nan() {
        return "nan".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "inf" } else { "-inf" }.into();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .into();
    }
    let scientific = format!("{value:e}");
    let (mantissa, exponent) = scientific.split_once('e').expect("an exponent");
    let exponent: i32 = exponent.parse().expect("a number");
    let negative = mantissa.starts_with('-');
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let sign = if negative { "-" } else { "" };
    if (-4..16).contains(&exponent) {
        let point = exponent + 1;
        let text = if point <= 0 {
            format!("0.{}{}", "0".repeat((-point) as usize), digits)
        } else if point as usize >= digits.len() {
            format!("{}{}.0", digits, "0".repeat(point as usize - digits.len()))
        } else {
            let (whole, fraction) = digits.split_at(point as usize);
            format!("{whole}.{fraction}")
        };
        format!("{sign}{text}")
    } else {
        let mantissa = if digits.len() == 1 {
            digits
        } else {
            format!("{}.{}", &digits[..1], &digits[1..])
        };
        let exponent_sign = if exponent < 0 { '-' } else { '+' };
        format!("{sign}{mantissa}e{exponent_sign}{:02}", exponent.abs())
    }
}

/// `int(text, base)`: optional sign, underscores between digits, and a prefix for base 0.
pub fn parse_int(text: &str, base: u32) -> Result<Value, Error> {
    let invalid = || {
        Error::value_error(format!(
            "invalid literal for int() with base {base}: {}",
            crate::format::quote(text)
        ))
    };
    let trimmed = text.trim();
    let (negative, digits) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    let lower = digits.to_ascii_lowercase();
    let (base, digits) = match base {
        0 | 16 if lower.starts_with("0x") => (16, &digits[2..]),
        0 | 8 if lower.starts_with("0o") => (8, &digits[2..]),
        0 | 2 if lower.starts_with("0b") => (2, &digits[2..]),
        0 => (10, digits),
        other => (other, digits),
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
    {
        return Err(invalid());
    }
    let cleaned: String = digits.chars().filter(|c| *c != '_').collect();
    let value = BigInt::parse_bytes(cleaned.as_bytes(), base).ok_or_else(invalid)?;
    Ok(Value::from_bigint(if negative { -value } else { value }))
}

/// `float(text)`: decimal numbers, `inf`, `nan`, with optional sign and surrounding space.
pub fn parse_float(text: &str) -> Result<f64, Error> {
    let trimmed = text.trim();
    let lower = trimmed.to_ascii_lowercase();
    let unsigned = lower.trim_start_matches(['+', '-']);
    let negative = lower.starts_with('-');
    let special = match unsigned {
        "inf" | "infinity" => Some(f64::INFINITY),
        "nan" => Some(f64::NAN),
        _ => None,
    };
    if let Some(value) = special {
        return Ok(if negative { -value } else { value });
    }
    let valid = !unsigned.is_empty()
        && unsigned
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | '+' | '-' | '_'))
        && unsigned.chars().any(|c| c.is_ascii_digit());
    let cleaned: String = trimmed.chars().filter(|c| *c != '_').collect();
    match cleaned.parse::<f64>() {
        Ok(value) if valid => Ok(value),
        _ => Err(Error::value_error(format!(
            "could not convert string to float: {}",
            crate::format::quote(text)
        ))),
    }
}

/// `round(x)` with banker's rounding, as Python does.
pub fn round_half_even(value: f64) -> f64 {
    let rounded = value.round();
    if (value - value.trunc()).abs() == 0.5 {
        2.0 * (value / 2.0).round()
    } else {
        rounded
    }
}

/// A float as an integer value, for `int(x)` and friends.
pub fn float_to_int(value: f64) -> Result<Value, Error> {
    if value.is_nan() {
        return Err(Error::value_error("cannot convert float NaN to integer"));
    }
    if value.is_infinite() {
        return Err(Error::new(
            "OverflowError",
            "cannot convert float infinity to integer",
        ));
    }
    let truncated = value.trunc();
    if truncated.abs() < 9.0e18 {
        Ok(Value::Int(truncated as i64))
    } else {
        Ok(Value::from_bigint(
            num_traits::FromPrimitive::from_f64(truncated).unwrap_or_default(),
        ))
    }
}
