//! Numbers as nf-co2footprint prints them for people: SI prefixes, durations in words,
//! scientific notation and ISO 8601 durations.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

/// The SI prefixes from pico to exa, a step of 1000 each.
const PREFIXES: [&str; 11] = ["p", "n", "u", "m", "", "k", "M", "G", "T", "P", "E"];

/// The units of a duration, largest first, with their length in milliseconds.
const UNITS: [(&str, &str, f64); 8] = [
    ("years", "year", 12.0 * 4.35 * 7.0 * 86_400_000.0),
    ("months", "month", 4.35 * 7.0 * 86_400_000.0),
    ("weeks", "week", 7.0 * 86_400_000.0),
    ("days", "day", 86_400_000.0),
    ("h", "h", 3_600_000.0),
    ("min", "min", 60_000.0),
    ("s", "s", 1000.0),
    ("ms", "ms", 1.0),
];

/// A quantity with the SI prefix that puts it at 1 or above, rounded to two decimals, like
/// `175.71 uWh` or `372.5 mW`.
pub(super) fn quantity(value: f64, unit: &str) -> String {
    if value == 0.0 || !value.is_finite() {
        return format!("{} {unit}", plain(value));
    }
    let step = (value.abs().log10() / 3.0).floor() as i32;
    let step = step.clamp(-4, 6);
    let scaled = value / 1000_f64.powi(step);
    let prefix = PREFIXES[(step + 4) as usize];
    format!("{} {prefix}{unit}", plain(scaled))
}

/// A number rounded half up to two decimals, without trailing zeros, like `2` or `244.89`.
pub(super) fn plain(value: f64) -> String {
    let rounded = (value * 100.0).round() / 100.0;
    let text = format!("{rounded:.2}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" {
        "0".to_owned()
    } else {
        text.to_owned()
    }
}

/// A share of CPU, like `2 %`.
pub(super) fn percent(value: f64) -> String {
    format!("{} %", plain(value))
}

/// A duration in words from milliseconds, like `1s 7ms`, with at most `parts` parts; the last
/// part keeps two decimals.
pub(super) fn duration(millis: f64, parts: usize) -> String {
    let mut texts = Vec::new();
    let mut left = millis.max(0.0);
    for (index, (plural, single, length)) in UNITS.iter().enumerate() {
        let last = index + 1 == UNITS.len() || texts.len() + 1 == parts;
        if left < *length && !(last && texts.is_empty() && index + 1 == UNITS.len()) {
            continue;
        }
        let amount = if last {
            left / length
        } else {
            (left / length).floor()
        };
        let name = if amount == 1.0 { single } else { plural };
        texts.push(format!("{}{name}", plain(amount)));
        left -= amount * length;
        if last || left <= 0.0 {
            break;
        }
    }
    texts.join(" ")
}

/// A number in scientific notation as the summary prints car and flight shares: plain from
/// 0.001 to 999 with up to three decimals, else like `1.31E-4`.
pub(super) fn scientific(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    if (0.001..=999.0).contains(&value.abs()) {
        let text = format!("{value:.3}");
        return text.trim_end_matches('0').trim_end_matches('.').to_owned();
    }
    let exponent = value.abs().log10().floor() as i32;
    let mantissa = value / 10_f64.powi(exponent);
    let (mantissa, exponent) = match (mantissa * 100.0).round() / 100.0 {
        ten if ten.abs() >= 10.0 => (ten / 10.0, exponent + 1),
        rounded => (rounded, exponent),
    };
    format!("{mantissa:.2}E{exponent}")
}

/// A duration as Java's `Duration` prints it, like `PT1.007S` or `PT1M5.7S`.
pub(super) fn iso_duration(millis: f64) -> String {
    let millis = millis.max(0.0).round() as u64;
    let (hours, minutes) = (millis / 3_600_000, millis / 60_000 % 60);
    let (seconds, rest) = (millis / 1000 % 60, millis % 1000);
    let mut text = "PT".to_owned();
    if hours > 0 {
        text.push_str(&format!("{hours}H"));
    }
    if minutes > 0 {
        text.push_str(&format!("{minutes}M"));
    }
    if seconds > 0 || rest > 0 || text == "PT" {
        let fraction = format!("{rest:03}");
        let fraction = fraction.trim_end_matches('0');
        match fraction.is_empty() {
            true => text.push_str(&format!("{seconds}S")),
            false => text.push_str(&format!("{seconds}.{fraction}S")),
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prints_recorded_values() {
        assert_eq!(quantity(0.000_175_712_766, "Wh"), "175.71 uWh");
        assert_eq!(quantity(0.3725, "W"), "372.5 mW");
        assert_eq!(quantity(1_073_741_824.0, "B"), "1.07 GB");
        assert_eq!(
            quantity(333.77, "gCO\u{2082}e/kWh"),
            "333.77 gCO\u{2082}e/kWh"
        );
        assert_eq!(quantity(0.02, "g"), "20 mg");
        assert_eq!(percent(244.894_616), "244.89 %");
        assert_eq!(duration(1007.0, usize::MAX), "1s 7ms");
        assert_eq!(duration(8755.0, usize::MAX), "8s 755ms");
        assert_eq!(duration(65_730.0, 2), "1min 5.73s");
        assert_eq!(scientific(0.022_911 / 175.0), "1.31E-4");
        assert_eq!(scientific(0.022_8 / 175.0), "1.30E-4");
        assert_eq!(scientific(0.022_911 * 100.0 / 50_000.0), "4.58E-5");
        assert_eq!(iso_duration(1007.0), "PT1.007S");
        assert_eq!(iso_duration(65_700.0), "PT1M5.7S");
    }
}
