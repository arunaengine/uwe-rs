//! Java's `Double.toString` and `Float.toString`, as of JDK 19.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt::LowerExp;
use std::str::FromStr;

/// Java's text of a `double`.
pub(super) fn double(value: f64) -> String {
    java_layout(
        value,
        value.is_nan(),
        value.is_infinite(),
        value.is_sign_negative(),
    )
}

/// Java's text of a `float`.
pub(super) fn float(value: f32) -> String {
    java_layout(
        value,
        value.is_nan(),
        value.is_infinite(),
        value.is_sign_negative(),
    )
}

/// Plain notation for magnitudes from 10^-3 up to 10^7, else `d.dddE<exponent>`; always with
/// at least one fraction digit.
fn java_layout<T>(value: T, nan: bool, infinite: bool, negative: bool) -> String
where
    T: LowerExp + FromStr + PartialEq + Copy,
{
    if nan {
        return "NaN".into();
    }
    let sign = if negative { "-" } else { "" };
    if infinite {
        return format!("{sign}Infinity");
    }
    let (digits, exponent) = shortest(value);
    if digits == "0" {
        return format!("{sign}0.0");
    }
    if (-3..7).contains(&exponent) {
        if exponent < 0 {
            let zeros = "0".repeat((-exponent - 1) as usize);
            return format!("{sign}0.{zeros}{digits}");
        }
        let whole = exponent as usize + 1;
        if digits.len() <= whole {
            let zeros = "0".repeat(whole - digits.len());
            return format!("{sign}{digits}{zeros}.0");
        }
        return format!("{sign}{}.{}", &digits[..whole], &digits[whole..]);
    }
    let fraction = if digits.len() > 1 { &digits[1..] } else { "0" };
    format!("{sign}{}.{fraction}E{exponent}", &digits[..1])
}

/// The digits and decimal exponent Java prints: the shortest decimal that reads back as the
/// value, except that a one-digit decimal gives way to a closer two-digit one.
fn shortest<T>(value: T) -> (String, i32)
where
    T: LowerExp + FromStr + PartialEq + Copy,
{
    let (digits, exponent) = split(&format!("{value:e}"));
    if digits.len() == 1 && digits != "0" {
        let text = format!("{value:.1e}");
        if text.parse::<T>().ok() == Some(value) {
            let (closer, exponent) = split(&text);
            let closer = closer.trim_end_matches('0');
            return (closer.to_owned(), exponent);
        }
    }
    (digits, exponent)
}

/// Splits Rust's `d.ddde<exponent>` into the digits without sign or point, and the exponent.
fn split(text: &str) -> (String, i32) {
    let text = text.trim_start_matches('-');
    let (mantissa, exponent) = text.split_once('e').unwrap_or((text, "0"));
    let digits = mantissa.replace('.', "");
    (digits, exponent.parse().unwrap_or(0))
}
