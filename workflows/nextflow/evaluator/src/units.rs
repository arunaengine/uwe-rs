//! Nextflow's `MemoryUnit` (bytes) and `Duration` (milliseconds): parsing, text and arithmetic.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::exception::Exception;
use crate::number::Number;

/// Memory units in order; a unit is worth `1024^index` bytes.
const MEMORY_UNITS: [&str; 8] = ["B", "KB", "MB", "GB", "TB", "PB", "EB", "ZB"];

/// Duration units and their milliseconds, as `Duration.convert` checks them.
const DURATION_UNITS: [(&str, f64); 17] = [
    ("ms", 1.0),
    ("milli", 1.0),
    ("millis", 1.0),
    ("s", 1e3),
    ("sec", 1e3),
    ("second", 1e3),
    ("seconds", 1e3),
    ("m", 6e4),
    ("min", 6e4),
    ("minute", 6e4),
    ("minutes", 6e4),
    ("h", 3.6e6),
    ("hour", 3.6e6),
    ("hours", 3.6e6),
    ("d", 8.64e7),
    ("day", 8.64e7),
    ("days", 8.64e7),
];

/// `new MemoryUnit(String)`: the whole text must match `([0-9\.]+)\s*(\S)?B?`. Without a unit,
/// or with `B`, the number must be an integer; otherwise it may have a fraction.
pub fn parse_memory(text: &str) -> Result<i64, Exception> {
    let invalid = || Exception::illegal_argument(format!("Not a valid FileSize value: '{text}'"));
    let digits = text
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(text.len());
    if digits == 0 {
        return Err(invalid());
    }
    let (number, rest) = text.split_at(digits);
    let rest = rest.trim_start_matches(java_space);
    let mut characters = rest.chars();
    let unit = match (characters.next(), characters.next(), characters.next()) {
        (None, _, _) => None,
        (Some(unit), None, _) if !java_space(unit) => Some(unit),
        (Some(unit), Some('B'), None) if !java_space(unit) => Some(unit),
        _ => return Err(invalid()),
    };
    let unit = unit.map(|unit| unit.to_uppercase().collect::<String>());
    match unit.as_deref() {
        None | Some("B") => number.parse().map_err(|_| Exception::number_format(number)),
        Some(unit) => {
            let index = MEMORY_UNITS
                .iter()
                .position(|name| *name == unit || *name == format!("{unit}B"))
                .ok_or_else(|| {
                    Exception::illegal_argument(format!("Not a valid file size unit: {text}"))
                })?;
            let value: f64 = number
                .parse()
                .map_err(|_| Exception::number_format(number))?;
            Ok(java_round(value * 1024f64.powi(index as i32)))
        }
    }
}

/// `MemoryUnit.toString()`: the largest unit that keeps the value at least 1, with at most one
/// fraction digit rounded half to even, like `1.5 GB`; zero is `0`.
pub fn memory_text(size: i64) -> String {
    if size <= 0 {
        return "0".into();
    }
    let groups = ((size as f64).log10() / 1024f64.log10()) as usize;
    let value = size as f64 / 1024f64.powi(groups as i32);
    let text = format!("{value:.1}");
    let text = text.strip_suffix(".0").unwrap_or(&text);
    format!("{text} {}", MEMORY_UNITS[groups])
}

/// `new Duration(String)`: parts like `1h 30m` or `1.5h`, else the legacy form `hh:mm:ss`.
pub fn parse_duration(text: &str) -> Result<i64, Exception> {
    let invalid = || Exception::illegal_argument(format!("Not a valid duration value: {text}"));
    match parse_parts(text) {
        Ok(Some(millis)) => Ok(millis),
        Ok(None) => parse_legacy(text).ok_or_else(invalid),
        // An unknown unit name is not retried in the legacy form.
        Err(()) => Err(invalid()),
    }
}

/// Repeatedly reads `(\d+\.?\d*)\s*([a-zA-Z]+)` from the start. `None` when the first part
/// does not match or text is left over; an error for an unknown unit.
fn parse_parts(text: &str) -> Result<Option<i64>, ()> {
    let mut rest = text;
    let mut total: i64 = 0;
    let mut first = true;
    loop {
        let digits = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        let mut end = digits;
        if digits > 0 && rest[digits..].starts_with('.') {
            end += 1;
            end += rest[end..]
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len() - end);
        }
        let after = rest[end..].trim_start_matches(java_space);
        let letters = after
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(after.len());
        if digits == 0 || letters == 0 {
            return Ok(if first || !rest.is_empty() {
                None
            } else {
                Some(total)
            });
        }
        let number: f64 = rest[..end].parse().map_err(|_| ())?;
        let factor = duration_unit(&after[..letters]).ok_or(())?;
        total = total.wrapping_add(java_round(number * factor));
        // Java's `trim()` removes every character up to the space.
        rest = after[letters..].trim_matches(|c: char| c <= ' ');
        first = false;
    }
}

/// `hh:mm:ss` with one or two digits per part.
fn parse_legacy(text: &str) -> Option<i64> {
    let parts: Vec<&str> = text.split(':').collect();
    let [hours, minutes, seconds] = parts[..] else {
        return None;
    };
    let mut millis = 0;
    for (part, factor) in [(hours, 3_600_000), (minutes, 60_000), (seconds, 1_000)] {
        if !(1..=2).contains(&part.len()) || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        millis += part.parse::<i64>().ok()? * factor;
    }
    Some(millis)
}

/// `Duration.toString()`: milliseconds below one second, seconds with at most one decimal
/// below one minute, else the non-zero parts of `1d 2h 3m 4s` with seconds rounded.
pub fn duration_text(millis: i64) -> String {
    if millis < 1_000 {
        return format!("{millis}ms");
    }
    if millis < 60_000 {
        let tenths = Number::Long(java_round(millis as f64 / 100.0));
        let seconds = tenths
            .divide(&Number::Integer(10))
            .expect("divisor is not zero");
        return format!("{seconds}s");
    }
    let seconds = java_round(millis as f64 / 1_000.0);
    let parts = [
        (seconds / 86_400, "d"),
        (seconds / 3_600 % 24, "h"),
        (seconds / 60 % 60, "m"),
        (seconds % 60, "s"),
    ];
    let parts: Vec<String> = parts
        .iter()
        .filter(|(count, _)| *count != 0)
        .map(|(count, unit)| format!("{count}{unit}"))
        .collect();
    parts.join(" ")
}

/// The milliseconds of a duration unit name, for number properties like `2.h`.
pub fn duration_unit(name: &str) -> Option<f64> {
    DURATION_UNITS
        .iter()
        .find(|(unit, _)| *unit == name)
        .map(|(_, factor)| *factor)
}

/// Whether the name is a memory unit, for number properties like `2.GB`.
pub fn is_memory_unit(name: &str) -> bool {
    MEMORY_UNITS.contains(&name)
}

/// A new unit count, which Nextflow asserts is not negative.
pub fn check_count(count: i64, what: &str) -> Result<i64, Exception> {
    if count < 0 {
        return Err(Exception::assertion(format!(
            "{what} unit cannot be a negative number"
        )));
    }
    Ok(count)
}

/// `unit * n`: `(long)(count * n)` in Groovy arithmetic, so a fraction is truncated.
pub fn multiply_count(count: i64, factor: &Number, what: &str) -> Result<i64, Exception> {
    check_count(Number::Long(count).multiply(factor).to_i64(), what)
}

/// `unit / n`: `Math.round((double)(count / n))` in Groovy arithmetic.
pub fn divide_count(count: i64, divisor: &Number, what: &str) -> Result<i64, Exception> {
    let quotient = Number::Long(count).divide(divisor)?;
    check_count(java_round(quotient.to_f64()), what)
}

/// Java's `\s` in regular expressions.
fn java_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r')
}

/// Java's `Math.round(double)`: the nearest `long`, halves toward positive infinity, `NaN` to 0,
/// and values beyond the range to its ends.
pub fn java_round(value: f64) -> i64 {
    let rounded = value.round();
    if rounded - value == -0.5 && value < 0.0 {
        (rounded + 1.0) as i64
    } else {
        rounded as i64
    }
}
