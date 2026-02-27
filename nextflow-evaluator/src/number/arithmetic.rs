//! Groovy's arithmetic on numbers: the math of both operands decides the class of the result.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cmp::Ordering;
use std::fmt;

use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive, Zero};

use super::{BigDecimal, Number};

/// A Java exception from arithmetic, with Java's message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArithmeticError {
    /// `java.lang.ArithmeticException`, such as a division by zero.
    Arithmetic(String),
    /// `java.lang.UnsupportedOperationException`, such as `intdiv` on a `BigDecimal`.
    Unsupported(String),
}

impl fmt::Display for ArithmeticError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArithmeticError::Arithmetic(message) | ArithmeticError::Unsupported(message) => {
                f.write_str(message)
            }
        }
    }
}

impl std::error::Error for ArithmeticError {}

/// The "math" Groovy picks for an operation, from the classes of both operands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Math {
    Integer,
    Long,
    BigInteger,
    BigDecimal,
    Floating,
}

fn math_of(number: &Number) -> Math {
    match number {
        Number::Integer(_) => Math::Integer,
        Number::Long(_) => Math::Long,
        Number::BigInteger(_) => Math::BigInteger,
        Number::BigDecimal(_) => Math::BigDecimal,
        Number::Double(_) | Number::Float(_) => Math::Floating,
    }
}

impl Number {
    pub fn add(&self, other: &Number) -> Number {
        match math_of(self).max(math_of(other)) {
            Math::Integer => Number::Integer(self.to_i32().wrapping_add(other.to_i32())),
            Math::Long => Number::Long(self.to_i64().wrapping_add(other.to_i64())),
            Math::BigInteger => Number::BigInteger(self.to_bigint() + other.to_bigint()),
            Math::BigDecimal => {
                Number::BigDecimal(self.to_big_decimal().add(&other.to_big_decimal()))
            }
            Math::Floating => Number::Double(self.to_f64() + other.to_f64()),
        }
    }

    pub fn subtract(&self, other: &Number) -> Number {
        match math_of(self).max(math_of(other)) {
            Math::Integer => Number::Integer(self.to_i32().wrapping_sub(other.to_i32())),
            Math::Long => Number::Long(self.to_i64().wrapping_sub(other.to_i64())),
            Math::BigInteger => Number::BigInteger(self.to_bigint() - other.to_bigint()),
            Math::BigDecimal => {
                Number::BigDecimal(self.to_big_decimal().subtract(&other.to_big_decimal()))
            }
            Math::Floating => Number::Double(self.to_f64() - other.to_f64()),
        }
    }

    pub fn multiply(&self, other: &Number) -> Number {
        match math_of(self).max(math_of(other)) {
            Math::Integer => Number::Integer(self.to_i32().wrapping_mul(other.to_i32())),
            Math::Long => Number::Long(self.to_i64().wrapping_mul(other.to_i64())),
            Math::BigInteger => Number::BigInteger(self.to_bigint() * other.to_bigint()),
            Math::BigDecimal => {
                Number::BigDecimal(self.to_big_decimal().multiply(&other.to_big_decimal()))
            }
            Math::Floating => Number::Double(self.to_f64() * other.to_f64()),
        }
    }

    /// `/`: a `Double` quotient if either side is floating point, else a `BigDecimal` quotient.
    pub fn divide(&self, other: &Number) -> Result<Number, ArithmeticError> {
        if math_of(self).max(math_of(other)) == Math::Floating {
            return Ok(Number::Double(self.to_f64() / other.to_f64()));
        }
        self.to_big_decimal()
            .groovy_divide(&other.to_big_decimal())
            .map(Number::BigDecimal)
            .map_err(|error| ArithmeticError::Arithmetic(error.to_string()))
    }

    /// `%`: Java's remainder for `Integer`, `Long`, `BigDecimal` and `Double`, and the never
    /// negative `BigInteger.mod` when either side is a `BigInteger`.
    pub fn modulo(&self, other: &Number) -> Result<Number, ArithmeticError> {
        let by_zero = || ArithmeticError::Arithmetic("/ by zero".into());
        Ok(match math_of(self).max(math_of(other)) {
            Math::Integer | Math::Long if other.is_zero() => return Err(by_zero()),
            Math::Integer => Number::Integer(self.to_i32().wrapping_rem(other.to_i32())),
            Math::Long => Number::Long(self.to_i64().wrapping_rem(other.to_i64())),
            Math::BigInteger => {
                let modulus = other.to_bigint();
                if !modulus.is_positive() {
                    return Err(ArithmeticError::Arithmetic(
                        "BigInteger: modulus not positive".into(),
                    ));
                }
                let remainder = self.to_bigint() % &modulus;
                Number::BigInteger(if remainder.is_negative() {
                    remainder + modulus
                } else {
                    remainder
                })
            }
            Math::BigDecimal => Number::BigDecimal(
                self.to_big_decimal()
                    .remainder(&other.to_big_decimal())
                    .map_err(|error| ArithmeticError::Arithmetic(error.to_string()))?,
            ),
            Math::Floating => Number::Double(self.to_f64() % other.to_f64()),
        })
    }

    /// `intdiv`: integer division truncated toward zero, only for integer classes.
    pub fn intdiv(&self, other: &Number) -> Result<Number, ArithmeticError> {
        let by_zero = || ArithmeticError::Arithmetic("/ by zero".into());
        match math_of(self).max(math_of(other)) {
            Math::Integer | Math::Long | Math::BigInteger if other.is_zero() => Err(by_zero()),
            Math::Integer => Ok(Number::Integer(self.to_i32().wrapping_div(other.to_i32()))),
            Math::Long => Ok(Number::Long(self.to_i64().wrapping_div(other.to_i64()))),
            Math::BigInteger => Ok(Number::BigInteger(self.to_bigint() / other.to_bigint())),
            _ => Err(unsupported("intdiv()", self)),
        }
    }

    /// `**`, with the overloads of Groovy's `DefaultGroovyMethods.power`.
    pub fn power(&self, exponent: &Number) -> Number {
        match (self, exponent) {
            (Number::Integer(base), Number::Integer(exponent)) if *exponent >= 0 => {
                let answer = BigInt::from(*base).pow(*exponent as u32);
                answer
                    .to_i32()
                    .map_or(Number::BigInteger(answer), Number::Integer)
            }
            (Number::Long(base), Number::Integer(exponent)) if *exponent >= 0 => {
                let answer = BigInt::from(*base).pow(*exponent as u32);
                answer
                    .to_i64()
                    .map_or(Number::BigInteger(answer), Number::Long)
            }
            (Number::BigInteger(base), Number::Integer(exponent)) if *exponent >= 0 => {
                Number::BigInteger(base.pow(*exponent as u32))
            }
            (Number::BigInteger(base), Number::BigInteger(exponent))
                if !exponent.is_negative() && exponent.to_i32().is_some() =>
            {
                Number::BigInteger(base.pow(exponent.to_u32().unwrap_or(0)))
            }
            (Number::BigDecimal(base), Number::Integer(exponent)) if *exponent >= 0 => {
                Number::BigDecimal(base.pow(*exponent as u32))
            }
            _ => {
                let answer = self.to_f64().powf(exponent.to_f64());
                if f64::from(answer as i32) == answer {
                    Number::Integer(answer as i32)
                } else if (answer as i64) as f64 == answer {
                    Number::Long(answer as i64)
                } else {
                    Number::Double(answer)
                }
            }
        }
    }

    /// Unary `-`, which keeps the class; `Integer` and `Long` wrap.
    pub fn negate(&self) -> Number {
        match self {
            Number::Integer(value) => Number::Integer(value.wrapping_neg()),
            Number::Long(value) => Number::Long(value.wrapping_neg()),
            Number::BigInteger(value) => Number::BigInteger(-value),
            Number::BigDecimal(value) => Number::BigDecimal(value.negate()),
            Number::Double(value) => Number::Double(-value),
            Number::Float(value) => Number::Float(-value),
        }
    }

    pub fn and(&self, other: &Number) -> Result<Number, ArithmeticError> {
        self.bits(other, "and()", |a, b| a & b, |a, b| a & b, |a, b| a & b)
    }

    pub fn or(&self, other: &Number) -> Result<Number, ArithmeticError> {
        self.bits(other, "or()", |a, b| a | b, |a, b| a | b, |a, b| a | b)
    }

    pub fn xor(&self, other: &Number) -> Result<Number, ArithmeticError> {
        self.bits(other, "xor()", |a, b| a ^ b, |a, b| a ^ b, |a, b| a ^ b)
    }

    /// `~`, only for integer classes.
    pub fn bitwise_not(&self) -> Result<Number, ArithmeticError> {
        match self {
            Number::Integer(value) => Ok(Number::Integer(!value)),
            Number::Long(value) => Ok(Number::Long(!value)),
            Number::BigInteger(value) => Ok(Number::BigInteger(!value)),
            _ => Err(unsupported("bitwiseNegate()", self)),
        }
    }

    /// `<<`. The class of the left side decides; `Integer` and `Long` take the distance modulo
    /// 32 and 64, as Java does.
    pub fn shift_left(&self, distance: &Number) -> Result<Number, ArithmeticError> {
        let by = shift_distance(distance)?;
        match self {
            Number::Integer(value) => Ok(Number::Integer(value.wrapping_shl(by as u32))),
            Number::Long(value) => Ok(Number::Long(value.wrapping_shl(by as u32))),
            Number::BigInteger(value) if by >= 0 => Ok(Number::BigInteger(value << by)),
            Number::BigInteger(value) => Ok(Number::BigInteger(value >> by.unsigned_abs())),
            _ => Err(unsupported("leftShift()", self)),
        }
    }

    /// `>>`, which keeps the sign.
    pub fn shift_right(&self, distance: &Number) -> Result<Number, ArithmeticError> {
        let by = shift_distance(distance)?;
        match self {
            Number::Integer(value) => Ok(Number::Integer(value.wrapping_shr(by as u32))),
            Number::Long(value) => Ok(Number::Long(value.wrapping_shr(by as u32))),
            Number::BigInteger(value) if by >= 0 => Ok(Number::BigInteger(value >> by)),
            Number::BigInteger(value) => Ok(Number::BigInteger(value << by.unsigned_abs())),
            _ => Err(unsupported("rightShift()", self)),
        }
    }

    /// `>>>`, which fills with zeros; `BigInteger` does not support it.
    pub fn shift_right_unsigned(&self, distance: &Number) -> Result<Number, ArithmeticError> {
        let by = shift_distance(distance)? as u32;
        match self {
            Number::Integer(value) => Ok(Number::Integer((*value as u32).wrapping_shr(by) as i32)),
            Number::Long(value) => Ok(Number::Long((*value as u64).wrapping_shr(by) as i64)),
            _ => Err(unsupported("rightShiftUnsigned()", self)),
        }
    }

    /// `&`, `|` or `^` in the integer math of both operands.
    fn bits(
        &self,
        other: &Number,
        operation: &str,
        integer: fn(i32, i32) -> i32,
        long: fn(i64, i64) -> i64,
        big: fn(&BigInt, &BigInt) -> BigInt,
    ) -> Result<Number, ArithmeticError> {
        match math_of(self).max(math_of(other)) {
            Math::Integer => Ok(Number::Integer(integer(self.to_i32(), other.to_i32()))),
            Math::Long => Ok(Number::Long(long(self.to_i64(), other.to_i64()))),
            Math::BigInteger => Ok(Number::BigInteger(big(
                &self.to_bigint(),
                &other.to_bigint(),
            ))),
            _ => Err(unsupported(operation, self)),
        }
    }

    /// Groovy's `compareTo` of two numbers, in the math of both operands. Floating point uses
    /// Java's `Double.compare`: `-0.0` is below `0.0`, and `NaN` is above everything.
    pub fn compare(&self, other: &Number) -> Ordering {
        match math_of(self).max(math_of(other)) {
            Math::Integer => self.to_i32().cmp(&other.to_i32()),
            Math::Long => self.to_i64().cmp(&other.to_i64()),
            Math::BigInteger => self.to_bigint().cmp(&other.to_bigint()),
            Math::BigDecimal => self.to_big_decimal().compare(&other.to_big_decimal()),
            Math::Floating => java_compare(self.to_f64(), other.to_f64()),
        }
    }

    /// Java's `intValue()`: truncated toward zero, then the lowest 32 bits.
    pub fn to_i32(&self) -> i32 {
        match self {
            Number::Integer(value) => *value,
            Number::Double(value) => *value as i32,
            Number::Float(value) => *value as i32,
            other => other.to_i64() as i32,
        }
    }

    /// Java's `longValue()`: truncated toward zero, then the lowest 64 bits.
    pub fn to_i64(&self) -> i64 {
        match self {
            Number::Integer(value) => i64::from(*value),
            Number::Long(value) => *value,
            Number::BigInteger(value) => super::low_bits(value),
            Number::BigDecimal(value) => super::low_bits(&value.to_bigint()),
            Number::Double(value) => *value as i64,
            Number::Float(value) => *value as i64,
        }
    }

    /// Java's `doubleValue()`.
    pub fn to_f64(&self) -> f64 {
        match self {
            Number::Integer(value) => f64::from(*value),
            Number::Long(value) => *value as f64,
            Number::BigInteger(value) => value.to_f64().unwrap_or(f64::NAN),
            Number::BigDecimal(value) => value.to_f64(),
            Number::Double(value) => *value,
            Number::Float(value) => f64::from(*value),
        }
    }

    /// The integer value, truncated toward zero, as Groovy's `toBigInteger`.
    pub fn to_bigint(&self) -> BigInt {
        match self {
            Number::Integer(value) => BigInt::from(*value),
            Number::Long(value) => BigInt::from(*value),
            Number::BigInteger(value) => value.clone(),
            Number::BigDecimal(value) => value.to_bigint(),
            Number::Double(_) | Number::Float(_) => self.to_big_decimal().to_bigint(),
        }
    }

    /// Groovy's `toBigDecimal`: exact for integers, and from the Java text for floating point.
    pub fn to_big_decimal(&self) -> BigDecimal {
        match self {
            Number::BigDecimal(value) => value.clone(),
            Number::Double(_) | Number::Float(_) => {
                BigDecimal::parse(&self.to_string()).unwrap_or_else(|| BigDecimal::from(0))
            }
            other => BigDecimal::from(other.to_bigint()),
        }
    }

    pub fn is_zero(&self) -> bool {
        match self {
            Number::Integer(value) => *value == 0,
            Number::Long(value) => *value == 0,
            Number::BigInteger(value) => value.is_zero(),
            Number::BigDecimal(value) => value.signum() == 0,
            Number::Double(value) => *value == 0.0,
            Number::Float(value) => *value == 0.0,
        }
    }

    /// The fully qualified Java class name.
    pub fn java_class(&self) -> &'static str {
        match self {
            Number::BigInteger(_) => "java.math.BigInteger",
            Number::BigDecimal(_) => "java.math.BigDecimal",
            Number::Integer(_) => "java.lang.Integer",
            Number::Long(_) => "java.lang.Long",
            Number::Double(_) => "java.lang.Double",
            Number::Float(_) => "java.lang.Float",
        }
    }
}

/// Java's `Double.compare`: the order of the numbers, then of the bits with one canonical `NaN`.
fn java_compare(left: f64, right: f64) -> Ordering {
    let bits = |value: f64| {
        if value.is_nan() {
            0x7ff8_0000_0000_0000
        } else {
            value.to_bits() as i64
        }
    };
    match left.partial_cmp(&right) {
        Some(Ordering::Equal) | None => bits(left).cmp(&bits(right)),
        Some(order) => order,
    }
}

/// A shift distance must be an integer class; Java takes its `intValue()`.
fn shift_distance(distance: &Number) -> Result<i32, ArithmeticError> {
    match distance {
        Number::BigDecimal(_) | Number::Double(_) | Number::Float(_) => {
            Err(ArithmeticError::Unsupported(format!(
                "Shift distance must be an integral type, but {distance} ({}) was supplied",
                distance.java_class()
            )))
        }
        _ => Ok(distance.to_i32()),
    }
}

/// Groovy's message for an operation that a number class does not implement.
pub(super) fn unsupported(operation: &str, number: &Number) -> ArithmeticError {
    ArithmeticError::Unsupported(format!(
        "Cannot use {operation} on this number type: {} with value: {number}",
        number.java_class()
    ))
}
