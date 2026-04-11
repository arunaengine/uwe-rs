//! Times as Go formats them: `time.Time.String()`, Go layouts like `2006-01-02`, and the
//! strftime variables Argo derives from `workflow.creationTimestamp`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use jiff::Timestamp;
use jiff::civil::Weekday;
use jiff::tz::TimeZone;

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

fn weekday_name(weekday: Weekday) -> &'static str {
    match weekday {
        Weekday::Monday => "Monday",
        Weekday::Tuesday => "Tuesday",
        Weekday::Wednesday => "Wednesday",
        Weekday::Thursday => "Thursday",
        Weekday::Friday => "Friday",
        Weekday::Saturday => "Saturday",
        Weekday::Sunday => "Sunday",
    }
}

/// `time.Time.String()` in UTC, like `2026-09-28 10:00:00.5 +0000 UTC`.
pub fn go_time_string(time: Timestamp) -> String {
    go_format("2006-01-02 15:04:05.999999999 -0700 MST", time)
}

/// RFC 3339 in UTC without fractions, as Argo formats `workflow.creationTimestamp`.
pub fn rfc3339(time: Timestamp) -> String {
    go_format("2006-01-02T15:04:05Z07:00", time)
}

/// Formats a UTC time with a Go layout.
pub fn go_format(layout: &str, time: Timestamp) -> String {
    let zoned = time.to_zoned(TimeZone::UTC);
    let mut out = String::new();
    let mut rest = layout;
    while !rest.is_empty() {
        let month = zoned.month() as usize;
        let hour12 = match zoned.hour() % 12 {
            0 => 12,
            hour => hour,
        };
        let (text, length): (String, usize) = if rest.starts_with("January") {
            (MONTHS[month - 1].into(), 7)
        } else if rest.starts_with("Jan") {
            (MONTHS[month - 1][..3].into(), 3)
        } else if rest.starts_with("Monday") {
            (weekday_name(zoned.weekday()).into(), 6)
        } else if rest.starts_with("Mon") {
            (weekday_name(zoned.weekday())[..3].into(), 3)
        } else if rest.starts_with("MST") {
            ("UTC".into(), 3)
        } else if rest.starts_with("2006") {
            (format!("{:04}", zoned.year()), 4)
        } else if rest.starts_with("002") {
            (format!("{:03}", zoned.day_of_year()), 3)
        } else if rest.starts_with("01") {
            (format!("{month:02}"), 2)
        } else if rest.starts_with("02") {
            (format!("{:02}", zoned.day()), 2)
        } else if rest.starts_with("03") {
            (format!("{hour12:02}"), 2)
        } else if rest.starts_with("04") {
            (format!("{:02}", zoned.minute()), 2)
        } else if rest.starts_with("05") {
            (format!("{:02}", zoned.second()), 2)
        } else if rest.starts_with("06") {
            (format!("{:02}", zoned.year() % 100), 2)
        } else if rest.starts_with("15") {
            (format!("{:02}", zoned.hour()), 2)
        } else if rest.starts_with("_2") {
            (format!("{:>2}", zoned.day()), 2)
        } else if rest.starts_with("-07:00:00") {
            ("+00:00:00".into(), 9)
        } else if rest.starts_with("-070000") {
            ("+000000".into(), 7)
        } else if rest.starts_with("-07:00") {
            ("+00:00".into(), 6)
        } else if rest.starts_with("-0700") {
            ("+0000".into(), 5)
        } else if rest.starts_with("-07") {
            ("+00".into(), 3)
        } else if let Some(length) = ["Z07:00:00", "Z070000", "Z07:00", "Z0700", "Z07"]
            .iter()
            .find(|zone| rest.starts_with(**zone))
            .map(|zone| zone.len())
        {
            ("Z".into(), length)
        } else if rest.starts_with("PM") {
            ((if zoned.hour() >= 12 { "PM" } else { "AM" }).into(), 2)
        } else if rest.starts_with("pm") {
            ((if zoned.hour() >= 12 { "pm" } else { "am" }).into(), 2)
        } else if let Some((text, length)) = fraction(rest, zoned.subsec_nanosecond()) {
            (text, length)
        } else {
            let first = rest.chars().next().unwrap_or_default();
            match first {
                '1' => (month.to_string(), 1),
                '2' => (zoned.day().to_string(), 1),
                '3' => (hour12.to_string(), 1),
                '4' => (zoned.minute().to_string(), 1),
                '5' => (zoned.second().to_string(), 1),
                other => (other.to_string(), other.len_utf8()),
            }
        };
        out.push_str(&text);
        rest = &rest[length..];
    }
    out
}

/// A fractional second like `.000` (fixed digits) or `.999` (trailing zeros dropped).
fn fraction(layout: &str, nanoseconds: i32) -> Option<(String, usize)> {
    let mut characters = layout.chars();
    let separator = characters.next()?;
    if separator != '.' && separator != ',' {
        return None;
    }
    let digit = characters.next()?;
    if digit != '0' && digit != '9' {
        return None;
    }
    let count = 1 + characters.take_while(|c| *c == digit).count();
    if layout[1 + count..].starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let digits = format!("{nanoseconds:09}");
    let digits = &digits[..count.min(9)];
    let text = if digit == '9' {
        let trimmed = digits.trim_end_matches('0');
        if trimmed.is_empty() {
            String::new()
        } else {
            format!("{separator}{trimmed}")
        }
    } else {
        format!("{separator}{digits}")
    };
    Some((text, 1 + count))
}

/// The strftime conversions that Argo offers as `workflow.creationTimestamp.<letter>`.
pub fn strftime(time: Timestamp) -> Vec<(char, String)> {
    let zoned = time.to_zoned(TimeZone::UTC);
    let weekday = weekday_name(zoned.weekday());
    let month = MONTHS[zoned.month() as usize - 1];
    let hour12 = match zoned.hour() % 12 {
        0 => 12,
        hour => hour,
    };
    let sunday_based = zoned.weekday().to_sunday_zero_offset();
    let mut values = vec![
        ('A', weekday.to_owned()),
        ('a', weekday[..3].to_owned()),
        ('B', month.to_owned()),
        ('b', month[..3].to_owned()),
        ('C', format!("{:02}", zoned.year() / 100)),
        ('c', go_format("Mon Jan _2 15:04:05 2006", time)),
        ('D', go_format("01/02/06", time)),
        ('d', format!("{:02}", zoned.day())),
        ('e', format!("{:>2}", zoned.day())),
        ('F', go_format("2006-01-02", time)),
        ('H', format!("{:02}", zoned.hour())),
        ('I', format!("{hour12:02}")),
        ('j', format!("{:03}", zoned.day_of_year())),
        ('k', format!("{:>2}", zoned.hour())),
        ('l', format!("{hour12:>2}")),
        ('M', format!("{:02}", zoned.minute())),
        ('m', format!("{:02}", zoned.month())),
        ('n', "\n".to_owned()),
        (
            'p',
            (if zoned.hour() >= 12 { "PM" } else { "AM" }).to_owned(),
        ),
        ('R', go_format("15:04", time)),
        ('r', go_format("03:04:05 PM", time)),
        ('S', format!("{:02}", zoned.second())),
        ('T', go_format("15:04:05", time)),
        ('t', "\t".to_owned()),
        ('u', zoned.weekday().to_monday_one_offset().to_string()),
        ('w', sunday_based.to_string()),
        ('X', go_format("15:04:05", time)),
        ('x', go_format("01/02/06", time)),
        ('Y', format!("{:04}", zoned.year())),
        ('y', format!("{:02}", zoned.year() % 100)),
        ('Z', "UTC".to_owned()),
        ('z', "+0000".to_owned()),
    ];
    // Week numbers: weeks start on Sunday (%U) or Monday (%W).
    let day_of_year = i32::from(zoned.day_of_year()) - 1;
    let week = |offset: i32| (day_of_year + 7 - offset) / 7;
    let monday_based = i32::from(zoned.weekday().to_monday_zero_offset());
    values.push(('U', format!("{:02}", week(i32::from(sunday_based)))));
    values.push(('W', format!("{:02}", week(monday_based))));
    values.push(('V', format!("{:02}", zoned.date().iso_week_date().week())));
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_go_layouts() {
        let time: Timestamp = "2026-09-28T14:05:09.5Z".parse().expect("a time");
        assert_eq!(
            go_format("2006-01-02 15:04:05", time),
            "2026-09-28 14:05:09"
        );
        assert_eq!(go_format("Jan _2 3:04PM", time), "Sep 28 2:05PM");
        assert_eq!(go_time_string(time), "2026-09-28 14:05:09.5 +0000 UTC");
        assert_eq!(rfc3339(time), "2026-09-28T14:05:09Z");
        assert_eq!(go_format("2006", time), "2026");
        let values = strftime(time);
        let value = |letter: char| {
            values
                .iter()
                .find(|(key, _)| *key == letter)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(value('Y'), Some("2026"));
        assert_eq!(value('j'), Some("271"));
        assert_eq!(value('a'), Some("Mon"));
    }
}
