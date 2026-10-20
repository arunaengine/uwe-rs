//! `java.util.Date`: its text and Java's `SimpleDateFormat` patterns, in the local time zone.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::exception::Exception;

/// The current time in milliseconds since 1970.
pub fn now() -> i64 {
    Timestamp::now().as_millisecond()
}

/// `Date.toString()`, like `Sun Sep 27 21:28:17 CEST 2026`.
pub fn text(millis: i64) -> String {
    format(millis, "EEE MMM dd HH:mm:ss zzz yyyy").unwrap_or_default()
}

/// Groovy's `date.format(pattern)`: Java's `SimpleDateFormat` in the local time zone. Only
/// the common pattern letters are supported.
pub fn format(millis: i64, pattern: &str) -> Result<String, Exception> {
    let time = Timestamp::from_millisecond(millis)
        .map_err(|error| Exception::illegal_argument(error.to_string()))?
        .to_zoned(TimeZone::system());
    format_zoned(&time, pattern)
}

/// A time with a zone in a pattern of `SimpleDateFormat` or `DateTimeFormatter`.
pub fn format_zoned(time: &jiff::Zoned, pattern: &str) -> Result<String, Exception> {
    let mut result = String::new();
    let characters: Vec<char> = pattern.chars().collect();
    let mut index = 0;
    while index < characters.len() {
        let letter = characters[index];
        let count = characters[index..]
            .iter()
            .take_while(|c| **c == letter)
            .count();
        index += count;
        let part = match (letter, count) {
            ('\'', _) => {
                // Text in quotes stays as it is; two quotes are one quote.
                if count % 2 == 0 {
                    result.push_str(&"'".repeat(count / 2));
                    continue;
                }
                let end = characters[index..].iter().position(|c| *c == '\'');
                let end = end.map_or(characters.len(), |end| index + end);
                result.extend(&characters[index..end]);
                index = end + 1;
                continue;
            }
            ('y' | 'u', 2) => "%y",
            ('y' | 'u', _) => "%Y",
            ('M', 1) => "%-m",
            ('M', 2) => "%m",
            ('M', 3) => "%b",
            ('M', _) => "%B",
            ('d', 1) => "%-d",
            ('d', _) => "%d",
            ('H', 1) => "%-H",
            ('H', _) => "%H",
            ('h', 1) => "%-I",
            ('h', _) => "%I",
            ('m', 1) => "%-M",
            ('m', _) => "%M",
            ('s', 1) => "%-S",
            ('s', _) => "%S",
            ('S', _) => "%3f",
            ('E', 1..=3) => "%a",
            ('E', _) => "%A",
            ('a', _) => "%p",
            ('z', 1..=3) => "%Z",
            ('Z', _) => "%z",
            (letter, _) if letter.is_ascii_alphabetic() => {
                let message = format!("Illegal pattern character '{letter}'");
                return Err(Exception::illegal_argument(message));
            }
            (other, count) => {
                result.extend(std::iter::repeat_n(other, count));
                continue;
            }
        };
        result.push_str(&time.strftime(part).to_string());
    }
    Ok(result)
}
