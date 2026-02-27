//! Numbers with the runtime classes and rules of Java and Groovy.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod arithmetic;
mod decimal;
mod text;

use std::fmt;

use num_bigint::BigInt;
use num_traits::ToPrimitive;

pub use arithmetic::ArithmeticError;
pub use decimal::{BigDecimal, DivideError};

/// A number of one of the six Java classes a Nextflow script can hold. The class is visible:
/// `1L instanceof Integer` is false, and `Integer` 1 and `Long` 1 are different map keys.
#[derive(Clone, Debug)]
pub enum Number {
    Integer(i32),
    Long(i64),
    BigInteger(BigInt),
    BigDecimal(BigDecimal),
    Double(f64),
    Float(f32),
}

/// A number literal that Groovy rejects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiteralError {
    pub message: String,
}

impl fmt::Display for LiteralError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for LiteralError {}

impl Number {
    /// Reads an integer literal as Groovy's `Numbers.parseInteger` does: underscores are
    /// dropped, `0x`, `0b` and a leading `0` pick the radix, and `i`, `l` or `g` pick the class.
    /// Without a suffix the class is the narrowest of `Integer`, `Long` and `BigInteger`.
    pub fn parse_integer(text: &str) -> Result<Self, LiteralError> {
        let text = text.replace('_', "");
        let error = |message: String| LiteralError { message };
        let (radix, digits) = match text.as_bytes() {
            [b'0', b'x' | b'X', ..] => (16, &text[2..]),
            [b'0', b'b' | b'B', ..] => (2, &text[2..]),
            [b'0', _, ..] => (8, &text[..]),
            _ => (10, &text[..]),
        };
        let (digits, suffix) = match digits.as_bytes().last() {
            Some(last @ (b'i' | b'I' | b'l' | b'L' | b'g' | b'G')) => {
                (&digits[..digits.len() - 1], Some(last.to_ascii_lowercase()))
            }
            _ => (digits, None),
        };
        let value = BigInt::parse_bytes(digits.as_bytes(), radix)
            .ok_or_else(|| error(format!("Not a valid integer: {text}")))?;
        let limit =
            |class: &str| error(format!("Number of type {class} exceeds its limit: {text}"));
        Ok(match suffix {
            Some(b'i') if radix == 10 => {
                Number::Integer(value.to_i32().ok_or_else(|| limit("Integer"))?)
            }
            Some(b'i') => Number::Integer(low_bits(&value) as i32),
            Some(b'l') if radix == 10 => Number::Long(value.to_i64().ok_or_else(|| limit("Long"))?),
            Some(b'l') => Number::Long(low_bits(&value)),
            Some(_) => Number::BigInteger(value),
            None => match (value.to_i32(), value.to_i64()) {
                (Some(small), _) => Number::Integer(small),
                (None, Some(long)) => Number::Long(long),
                (None, None) => Number::BigInteger(value),
            },
        })
    }

    /// Reads a floating-point literal as Groovy's `Numbers.parseDecimal` does: `d` gives a
    /// `Double`, `f` a `Float`, and no suffix or `g` a `BigDecimal` that keeps the scale of the text.
    pub fn parse_decimal(text: &str) -> Result<Self, LiteralError> {
        let text = text.replace('_', "");
        let error = |message: &str| LiteralError {
            message: format!("{message}: {text}"),
        };
        let (digits, suffix) = match text.as_bytes().last() {
            Some(last @ (b'd' | b'D' | b'f' | b'F' | b'g' | b'G')) => {
                (&text[..text.len() - 1], Some(last.to_ascii_lowercase()))
            }
            _ => (&text[..], None),
        };
        let value = BigDecimal::parse(digits).ok_or_else(|| error("Not a valid number"))?;
        match suffix {
            Some(b'd') => {
                let double: f64 = digits.parse().map_err(|_| error("Not a valid number"))?;
                if double.is_infinite() {
                    return Err(error("Out of range"));
                }
                Ok(Number::Double(double))
            }
            Some(b'f') => {
                let float: f32 = digits.parse().map_err(|_| error("Not a valid number"))?;
                if float.is_infinite() {
                    return Err(error("Out of range"));
                }
                Ok(Number::Float(float))
            }
            _ => Ok(Number::BigDecimal(value)),
        }
    }

    /// The simple name of the Java class, as `getClass().getSimpleName()` gives it.
    pub fn class_name(&self) -> &'static str {
        match self {
            Number::Integer(_) => "Integer",
            Number::Long(_) => "Long",
            Number::BigInteger(_) => "BigInteger",
            Number::BigDecimal(_) => "BigDecimal",
            Number::Double(_) => "Double",
            Number::Float(_) => "Float",
        }
    }
}

/// The lowest 64 bits in two's complement, as Java's `BigInteger.longValue()`.
fn low_bits(value: &BigInt) -> i64 {
    let bytes = value.to_signed_bytes_le();
    let fill = if value.sign() == num_bigint::Sign::Minus {
        0xff
    } else {
        0
    };
    let mut word = [fill; 8];
    for (slot, byte) in word.iter_mut().zip(bytes) {
        *slot = byte;
    }
    i64::from_le_bytes(word)
}

/// Java's `toString()` of the number.
impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Number::Integer(value) => write!(f, "{value}"),
            Number::Long(value) => write!(f, "{value}"),
            Number::BigInteger(value) => write!(f, "{value}"),
            Number::BigDecimal(value) => write!(f, "{value}"),
            Number::Double(value) => f.write_str(&text::double(*value)),
            Number::Float(value) => f.write_str(&text::float(*value)),
        }
    }
}
