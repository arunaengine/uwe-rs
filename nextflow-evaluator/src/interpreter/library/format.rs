//! Java's `String.format`: `%[index$][flags][width][.precision]conversion` for the conversions
//! `s`, `d`, `f`, `x`, `o`, `c`, `b`, `n` and `%`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use num_traits::Signed;

use crate::exception::Exception;
use crate::number::Number;
use crate::value::Value;

/// The text of a format pattern with its arguments.
pub(super) fn format(pattern: &str, arguments: &[Value]) -> Result<String, Exception> {
    let mut out = String::new();
    let mut next = 0;
    let mut rest = pattern;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let spec = &rest[start + 1..];
        let end = spec
            .find(|c: char| c.is_ascii_alphabetic() || c == '%')
            .ok_or_else(|| unknown(spec.chars().last().unwrap_or('%')))?;
        let conversion = spec[end..].chars().next().unwrap_or('%');
        let text = &spec[..end];
        rest = &spec[end + conversion.len_utf8()..];
        let (index, text) = match text.split_once('$') {
            Some((index, text)) => (index.parse::<usize>().ok(), text),
            None => (None, text),
        };
        let flags: String = text
            .chars()
            .take_while(|c| "-#+ 0,(".contains(*c))
            .collect();
        let text = &text[flags.len()..];
        let (width, precision) = match text.split_once('.') {
            Some((width, precision)) => (width, precision.parse::<usize>().ok()),
            None => (text, None),
        };
        let width = width.parse::<usize>().unwrap_or(0);
        let body = match conversion {
            '%' => "%".to_owned(),
            'n' => "\n".to_owned(),
            _ => {
                let argument = match index {
                    Some(index) => arguments.get(index.wrapping_sub(1)),
                    None => {
                        next += 1;
                        arguments.get(next - 1)
                    }
                };
                let argument = argument.ok_or_else(|| {
                    Exception::new(
                        "MissingFormatArgumentException",
                        format!("Format specifier '%{text}{conversion}'"),
                    )
                })?;
                convert(argument, conversion, &flags, precision)?
            }
        };
        let body = if conversion.is_ascii_uppercase() {
            body.to_uppercase()
        } else {
            body
        };
        out.push_str(&pad(body, width, &flags));
    }
    out.push_str(rest);
    Ok(out)
}

/// One argument as a conversion writes it, before padding to the width.
fn convert(
    argument: &Value,
    conversion: char,
    flags: &str,
    precision: Option<usize>,
) -> Result<String, Exception> {
    let mismatch = || {
        Exception::new(
            "IllegalFormatConversionException",
            format!("{conversion} != {}", argument.java_class_name()),
        )
    };
    let number = match argument {
        Value::Number(number) => Some(number),
        _ => None,
    };
    Ok(match conversion.to_ascii_lowercase() {
        's' => {
            let text = match argument {
                Value::Null => "null".to_owned(),
                other => other.to_java_string(),
            };
            match precision {
                Some(precision) => text.chars().take(precision).collect(),
                None => text,
            }
        }
        'b' => match argument {
            Value::Null => "false".to_owned(),
            Value::Boolean(value) => value.to_string(),
            _ => "true".to_owned(),
        },
        'c' => match argument {
            Value::Number(number) => char::from_u32(number.to_i64() as u32)
                .ok_or_else(mismatch)?
                .to_string(),
            _ => return Err(mismatch()),
        },
        'd' => match number {
            Some(Number::Double(_) | Number::Float(_) | Number::BigDecimal(_)) | None => {
                return Err(mismatch());
            }
            Some(number) => signed(number.to_bigint().to_string(), flags),
        },
        'x' | 'o' => match number {
            Some(Number::Integer(value)) if conversion.eq_ignore_ascii_case(&'x') => {
                format!("{:x}", *value as u32)
            }
            Some(Number::Integer(value)) => format!("{:o}", *value as u32),
            Some(Number::Long(value)) if conversion.eq_ignore_ascii_case(&'x') => {
                format!("{:x}", *value as u64)
            }
            Some(Number::Long(value)) => format!("{:o}", *value as u64),
            Some(Number::BigInteger(value)) if conversion.eq_ignore_ascii_case(&'x') => {
                value.to_str_radix(16)
            }
            Some(Number::BigInteger(value)) => value.to_str_radix(8),
            _ => return Err(mismatch()),
        },
        'f' => match number {
            Some(number @ (Number::Double(_) | Number::Float(_) | Number::BigDecimal(_))) => {
                let value = number.to_f64();
                if value.is_nan() || value.is_infinite() {
                    return Ok(number.to_string());
                }
                // Java rounds the decimal text of the value half up.
                let rounded = number
                    .to_big_decimal()
                    .round_scale(precision.unwrap_or(6) as i32);
                let digits = rounded.unscaled().abs().to_string();
                let scale = rounded.scale() as usize;
                let digits = format!("{digits:0>width$}", width = scale + 1);
                let (whole, fraction) = digits.split_at(digits.len() - scale);
                let whole = if flags.contains(',') {
                    grouped(whole)
                } else {
                    whole.to_owned()
                };
                let text = match fraction {
                    "" => whole,
                    fraction => format!("{whole}.{fraction}"),
                };
                let sign = if rounded.unscaled().is_negative() {
                    "-"
                } else {
                    ""
                };
                signed(format!("{sign}{text}"), flags)
            }
            _ => return Err(mismatch()),
        },
        other => return Err(unknown(other)),
    })
}

/// A number with the `+` or space flag for positive values, and `,` groups for `d`.
fn signed(text: String, flags: &str) -> String {
    let (sign, digits) = match text.strip_prefix('-') {
        Some(digits) => ("-", digits.to_owned()),
        None if flags.contains('+') => ("+", text),
        None if flags.contains(' ') => (" ", text),
        None => ("", text),
    };
    let digits = if flags.contains(',') && !digits.contains('.') {
        grouped(&digits)
    } else {
        digits
    };
    format!("{sign}{digits}")
}

/// Digits with a comma between each group of three.
fn grouped(digits: &str) -> String {
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Pads to the width: on the right with `-`, with zeros after the sign with `0`, else with
/// spaces on the left.
fn pad(body: String, width: usize, flags: &str) -> String {
    let length = body.chars().count();
    if length >= width {
        return body;
    }
    let fill = width - length;
    if flags.contains('-') {
        format!("{body}{}", " ".repeat(fill))
    } else if flags.contains('0') {
        let split = usize::from(body.starts_with(['-', '+', ' ']));
        format!("{}{}{}", &body[..split], "0".repeat(fill), &body[split..])
    } else {
        format!("{}{body}", " ".repeat(fill))
    }
}

fn unknown(conversion: char) -> Exception {
    Exception::new(
        "UnknownFormatConversionException",
        format!("Conversion = '{conversion}'"),
    )
}
