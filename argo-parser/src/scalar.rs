//! Resolves plain YAML scalars with the YAML 1.1 rules of go-yaml v2, which Argo uses.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::node::Value;

/// The value of a plain scalar. go-yaml checks a fixed word list, then integers, then floats.
pub fn resolve_plain(text: &str) -> Value {
    if let Some(value) = resolve_word(text) {
        return value;
    }
    match text.as_bytes().first() {
        Some(b'.') => text
            .parse::<f64>()
            .map_or_else(|_| Value::String(text.to_owned()), Value::Float),
        Some(b'+' | b'-' | b'0'..=b'9') => resolve_number(text),
        _ => Value::String(text.to_owned()),
    }
}

fn resolve_word(text: &str) -> Option<Value> {
    Some(match text {
        "y" | "Y" | "yes" | "Yes" | "YES" | "true" | "True" | "TRUE" | "on" | "On" | "ON" => {
            Value::Bool(true)
        }
        "n" | "N" | "no" | "No" | "NO" | "false" | "False" | "FALSE" | "off" | "Off" | "OFF" => {
            Value::Bool(false)
        }
        "" | "~" | "null" | "Null" | "NULL" => Value::Null,
        ".nan" | ".NaN" | ".NAN" => Value::Float(f64::NAN),
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" => Value::Float(f64::INFINITY),
        "-.inf" | "-.Inf" | "-.INF" => Value::Float(f64::NEG_INFINITY),
        _ => return None,
    })
}

/// Numbers start with a digit or a sign. Underscores are dropped before parsing, as go-yaml does.
fn resolve_number(text: &str) -> Value {
    let plain: String = text.chars().filter(|&c| c != '_').collect();
    let (negative, digits) = match plain.as_bytes().first() {
        Some(b'-') => (true, &plain[1..]),
        Some(b'+') => (false, &plain[1..]),
        _ => (false, plain.as_str()),
    };
    if let Some(magnitude) = parse_magnitude(digits) {
        let signed = if negative {
            0i128 - i128::from(magnitude)
        } else {
            i128::from(magnitude)
        };
        if let Ok(number) = i64::try_from(signed) {
            return Value::Integer(number);
        }
        // Go's ParseUint rejects signs, so only an unsigned text reaches `u64`.
        if !plain.starts_with(['-', '+']) {
            return Value::Unsigned(magnitude);
        }
    }
    if is_float(&plain)
        && let Ok(number) = plain.parse::<f64>()
    {
        return Value::Float(number);
    }
    Value::String(text.to_owned())
}

/// Go's base prefixes for `strconv.ParseInt(text, 0, 64)`; a leading `0` means octal.
fn parse_magnitude(digits: &str) -> Option<u64> {
    let (radix, rest) = match digits.get(..2) {
        Some("0x" | "0X") => (16, &digits[2..]),
        Some("0o" | "0O") => (8, &digits[2..]),
        Some("0b" | "0B") => (2, &digits[2..]),
        _ if digits.len() > 1 && digits.starts_with('0') => (8, &digits[1..]),
        _ => (10, digits),
    };
    if rest.is_empty() || !rest.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    u64::from_str_radix(rest, radix).ok()
}

/// go-yaml's float pattern: `[-+]?(\.[0-9]+|[0-9]+(\.[0-9]*)?)([eE][-+]?[0-9]+)?`.
fn is_float(text: &str) -> bool {
    let text = text.strip_prefix(['-', '+']).unwrap_or(text);
    let (mantissa, exponent) = match text.find(['e', 'E']) {
        Some(index) => (&text[..index], Some(&text[index + 1..])),
        None => (text, None),
    };
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    let mantissa_fits = match mantissa.split_once('.') {
        Some(("", fraction)) => digits(fraction),
        Some((whole, fraction)) => {
            digits(whole) && (fraction.is_empty() || fraction.bytes().all(|b| b.is_ascii_digit()))
        }
        None => digits(mantissa),
    };
    let exponent_fits =
        exponent.is_none_or(|part| digits(part.strip_prefix(['-', '+']).unwrap_or(part)));
    mantissa_fits && exponent_fits
}

#[cfg(test)]
#[path = "scalar_tests.rs"]
mod tests;
