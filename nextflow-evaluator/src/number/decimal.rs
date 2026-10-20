//! Java's `BigDecimal`: an unscaled integer and a scale, with Java's text form and Groovy's division.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cmp::Ordering;
use std::fmt;

use num_bigint::{BigInt, Sign};
use num_traits::{Signed, Zero};

/// The value `unscaled × 10^-scale`. Two values with different scales are different numbers
/// in Java, although they compare as equal: `1.0` and `1.00`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BigDecimal {
    unscaled: BigInt,
    scale: i32,
}

/// The `ArithmeticException` of a division, with Java's messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DivideError {
    /// `x / 0` for `x` other than zero.
    ByZero,
    /// `0 / 0`.
    Undefined,
}

impl fmt::Display for DivideError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            DivideError::ByZero => "Division by zero",
            DivideError::Undefined => "Division undefined",
        })
    }
}

impl std::error::Error for DivideError {}

/// Extra digits and smallest scale of a division that does not terminate, as in Groovy.
const DIVISION_EXTRA_PRECISION: u32 = 10;
const DIVISION_MIN_SCALE: i32 = 10;

impl BigDecimal {
    pub fn new(unscaled: BigInt, scale: i32) -> Self {
        Self { unscaled, scale }
    }

    pub fn unscaled(&self) -> &BigInt {
        &self.unscaled
    }

    pub fn scale(&self) -> i32 {
        self.scale
    }

    /// Reads the text as Java's `new BigDecimal(String)` does: a sign, digits with an optional
    /// point, and an optional exponent. The scale is the number of fraction digits minus the exponent.
    pub fn parse(text: &str) -> Option<Self> {
        let (mantissa, exponent) = match text.find(['e', 'E']) {
            Some(at) => (&text[..at], text[at + 1..].parse::<i64>().ok()?),
            None => (text, 0),
        };
        let (negative, mantissa) = match mantissa.as_bytes().first()? {
            b'-' => (true, &mantissa[1..]),
            b'+' => (false, &mantissa[1..]),
            _ => (false, mantissa),
        };
        let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        let digits = [whole, fraction].concat();
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let scale = i32::try_from(fraction.len() as i64 - exponent).ok()?;
        let magnitude = BigInt::parse_bytes(digits.as_bytes(), 10)?;
        Some(Self::new(
            if negative { -magnitude } else { magnitude },
            scale,
        ))
    }

    /// The number of decimal digits of the unscaled value; zero has one digit.
    pub fn precision(&self) -> u32 {
        digits(&self.unscaled)
    }

    pub fn signum(&self) -> i32 {
        match self.unscaled.sign() {
            Sign::Minus => -1,
            Sign::NoSign => 0,
            Sign::Plus => 1,
        }
    }

    pub fn negate(&self) -> Self {
        Self::new(-&self.unscaled, self.scale)
    }

    /// The sum; its scale is the larger scale.
    pub fn add(&self, other: &Self) -> Self {
        let (left, right, scale) = aligned(self, other);
        Self::new(left + right, scale)
    }

    /// The difference; its scale is the larger scale.
    pub fn subtract(&self, other: &Self) -> Self {
        let (left, right, scale) = aligned(self, other);
        Self::new(left - right, scale)
    }

    /// The product; its scale is the sum of the scales.
    pub fn multiply(&self, other: &Self) -> Self {
        Self::new(&self.unscaled * &other.unscaled, self.scale + other.scale)
    }

    /// `this` to the power of `exponent`, exactly.
    pub fn pow(&self, exponent: u32) -> Self {
        let scale = i32::try_from(i64::from(self.scale) * i64::from(exponent)).unwrap_or(i32::MAX);
        Self::new(self.unscaled.pow(exponent), scale)
    }

    /// Java's `divide(BigDecimal)`: the exact quotient, with the smallest scale that is at least
    /// the difference of the scales. `None` when the quotient does not terminate.
    pub fn divide_exact(&self, divisor: &Self) -> Result<Option<Self>, DivideError> {
        check_divisor(self, divisor)?;
        let preferred = self.scale - divisor.scale;
        if self.unscaled.is_zero() {
            return Ok(Some(Self::new(BigInt::zero(), preferred)));
        }
        let mut rest = divisor.unscaled.abs();
        for factor in [2u32, 5] {
            while (&rest % factor).is_zero() {
                rest /= factor;
            }
        }
        if !(&self.unscaled % &rest).is_zero() {
            return Ok(None);
        }
        // The smallest power of ten that makes the division exact.
        let mut shift = 0;
        while !((&self.unscaled * ten_to(shift)) % &divisor.unscaled).is_zero() {
            shift += 1;
        }
        let quotient = &self.unscaled * ten_to(shift) / &divisor.unscaled;
        Ok(Some(Self::new(quotient, preferred + shift as i32)))
    }

    /// Java's `divide(BigDecimal, new MathContext(precision))`: the quotient rounded half up to
    /// `precision` digits, without trailing zeros beyond the difference of the scales.
    pub fn divide_rounded(&self, divisor: &Self, precision: u32) -> Result<Self, DivideError> {
        check_divisor(self, divisor)?;
        let preferred = self.scale - divisor.scale;
        if self.unscaled.is_zero() {
            return Ok(Self::new(BigInt::zero(), preferred));
        }
        // Shift so that the integer quotient has more than `precision` digits.
        let shift = i64::from(precision) + i64::from(digits(&divisor.unscaled))
            - i64::from(digits(&self.unscaled))
            + 1;
        let (numerator, denominator) = if shift >= 0 {
            (
                &self.unscaled * ten_to(shift as u32),
                divisor.unscaled.clone(),
            )
        } else {
            (
                self.unscaled.clone(),
                &divisor.unscaled * ten_to((-shift) as u32),
            )
        };
        let quotient = Self::new(numerator / denominator, preferred + shift as i32);
        Ok(quotient.round(precision).strip_zeros_to(preferred))
    }

    /// Groovy's `/` on integers and decimals: the exact quotient if it terminates, else a rounded
    /// quotient with at most `max(left scale, right scale, 10)` fraction digits.
    pub fn groovy_divide(&self, divisor: &Self) -> Result<Self, DivideError> {
        if let Some(quotient) = self.divide_exact(divisor)? {
            return Ok(quotient);
        }
        let precision = self.precision().max(divisor.precision()) + DIVISION_EXTRA_PRECISION;
        let quotient = self.divide_rounded(divisor, precision)?;
        let scale = self.scale.max(divisor.scale).max(DIVISION_MIN_SCALE);
        Ok(if quotient.scale > scale {
            quotient.round_scale(scale)
        } else {
            quotient
        })
    }

    /// Java's `remainder`: `this - q × divisor` where `q` is the quotient truncated toward zero,
    /// so the result has the sign of `this`.
    pub fn remainder(&self, divisor: &Self) -> Result<Self, DivideError> {
        check_divisor(self, divisor)?;
        let preferred = self.scale - divisor.scale;
        let (left, right, _) = aligned(self, divisor);
        let integral = if left.abs() < right.abs() {
            Self::new(BigInt::zero(), preferred)
        } else {
            let whole = Self::new(left / right, 0).strip_zeros_to(preferred);
            if whole.scale < preferred {
                whole.round_scale(preferred)
            } else {
                whole
            }
        };
        Ok(self.subtract(&integral.multiply(divisor)))
    }

    /// Java's `setScale(scale, RoundingMode.HALF_UP)`.
    pub fn round_scale(&self, scale: i32) -> Self {
        match scale.cmp(&self.scale) {
            Ordering::Equal => self.clone(),
            Ordering::Greater => {
                Self::new(&self.unscaled * ten_to((scale - self.scale) as u32), scale)
            }
            Ordering::Less => Self::new(
                round_half_up(&self.unscaled, (self.scale - scale) as u32),
                scale,
            ),
        }
    }

    /// Java's `setScale(scale, mode)` with a `RoundingMode` by name; `None` when the mode is
    /// unknown or is `UNNECESSARY` and the value would need rounding.
    pub fn set_scale(&self, scale: i32, mode: &str) -> Option<Self> {
        if scale >= self.scale {
            return Some(self.round_scale(scale));
        }
        let divisor = ten_to((self.scale - scale) as u32);
        let quotient = &self.unscaled / &divisor;
        let remainder = &self.unscaled % &divisor;
        if remainder.is_zero() {
            return Some(Self::new(quotient, scale));
        }
        let half = (remainder.abs() * BigInt::from(2)).cmp(&divisor);
        let away = match mode {
            "UP" => true,
            "DOWN" => false,
            "CEILING" => self.unscaled.is_positive(),
            "FLOOR" => self.unscaled.is_negative(),
            "HALF_UP" => half.is_ge(),
            "HALF_DOWN" => half.is_gt(),
            "HALF_EVEN" => {
                half.is_gt() || (half.is_eq() && !(&quotient % BigInt::from(2)).is_zero())
            }
            _ => return None,
        };
        let quotient = match away {
            true => quotient + self.unscaled.signum(),
            false => quotient,
        };
        Some(Self::new(quotient, scale))
    }

    /// Compares the values, ignoring the scales: `1.0` and `1.00` are equal.
    pub fn compare(&self, other: &Self) -> Ordering {
        let (left, right, _) = aligned(self, other);
        left.cmp(&right)
    }

    /// The integer part, truncated toward zero.
    pub fn to_bigint(&self) -> BigInt {
        if self.scale <= 0 {
            &self.unscaled * ten_to(self.scale.unsigned_abs())
        } else {
            &self.unscaled / ten_to(self.scale as u32)
        }
    }

    /// The nearest `double`, as Java's `doubleValue()`.
    pub fn to_f64(&self) -> f64 {
        self.to_string().parse().unwrap_or(f64::NAN)
    }

    /// Rounds half up to at most `precision` digits, as Java's `MathContext` rounding does.
    fn round(self, precision: u32) -> Self {
        let mut value = self;
        loop {
            let count = digits(&value.unscaled);
            if count <= precision {
                return value;
            }
            let drop = count - precision;
            value = Self::new(
                round_half_up(&value.unscaled, drop),
                value.scale - drop as i32,
            );
        }
    }

    /// Removes trailing zeros while the scale is above `preferred`.
    fn strip_zeros_to(self, preferred: i32) -> Self {
        let mut value = self;
        while value.scale > preferred && !value.unscaled.is_zero() {
            let ten = BigInt::from(10);
            if !(&value.unscaled % &ten).is_zero() {
                break;
            }
            value = Self::new(value.unscaled / ten, value.scale - 1);
        }
        value
    }
}

impl From<BigInt> for BigDecimal {
    fn from(value: BigInt) -> Self {
        Self::new(value, 0)
    }
}

impl From<i64> for BigDecimal {
    fn from(value: i64) -> Self {
        Self::new(BigInt::from(value), 0)
    }
}

/// Java's `BigDecimal.toString()`: plain notation when the scale is not negative and the
/// adjusted exponent is at least -6, else scientific notation like `1.59E+7`.
impl fmt::Display for BigDecimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let coefficient = self.unscaled.magnitude().to_string();
        let sign = if self.unscaled.is_negative() { "-" } else { "" };
        if self.scale == 0 {
            return write!(f, "{sign}{coefficient}");
        }
        let length = coefficient.len() as i64;
        let adjusted = -i64::from(self.scale) + length - 1;
        if self.scale > 0 && adjusted >= -6 {
            let scale = self.scale as i64;
            return if scale >= length {
                let zeros = "0".repeat((scale - length) as usize);
                write!(f, "{sign}0.{zeros}{coefficient}")
            } else {
                let (whole, fraction) = coefficient.split_at((length - scale) as usize);
                write!(f, "{sign}{whole}.{fraction}")
            };
        }
        let (first, rest) = coefficient.split_at(1);
        let point = if rest.is_empty() { "" } else { "." };
        let exponent_sign = if adjusted > 0 { "+" } else { "" };
        write!(f, "{sign}{first}{point}{rest}E{exponent_sign}{adjusted}")
    }
}

fn check_divisor(dividend: &BigDecimal, divisor: &BigDecimal) -> Result<(), DivideError> {
    match (divisor.unscaled.is_zero(), dividend.unscaled.is_zero()) {
        (true, true) => Err(DivideError::Undefined),
        (true, false) => Err(DivideError::ByZero),
        _ => Ok(()),
    }
}

/// Both unscaled values at the larger of the two scales, and that scale.
fn aligned(left: &BigDecimal, right: &BigDecimal) -> (BigInt, BigInt, i32) {
    let scale = left.scale.max(right.scale);
    let widen = |value: &BigDecimal| &value.unscaled * ten_to((scale - value.scale) as u32);
    (widen(left), widen(right), scale)
}

fn ten_to(exponent: u32) -> BigInt {
    BigInt::from(10).pow(exponent)
}

/// The number of decimal digits of the magnitude; zero has one digit.
fn digits(value: &BigInt) -> u32 {
    value.magnitude().to_string().len() as u32
}

/// Divides by `10^drop` and rounds half away from zero, as Java's `HALF_UP`.
fn round_half_up(value: &BigInt, drop: u32) -> BigInt {
    let divisor = ten_to(drop);
    let quotient = value / &divisor;
    let remainder = value % &divisor;
    if remainder.abs() * 2 >= divisor {
        quotient + value.signum()
    } else {
        quotient
    }
}
