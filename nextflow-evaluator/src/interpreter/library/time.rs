//! `java.time` values for timestamps: `LocalDate`, `LocalDateTime`, `Instant` and `Duration`,
//! with their factories, their text and the methods that scripts use.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp, ToSpan, Zoned};

use crate::exception::Exception;
use crate::number::Number;
use crate::value::{Object, State, Time, Value};

/// A `java.time` value as an object.
pub(in crate::interpreter) fn time_value(time: Time) -> Value {
    let class = match time {
        Time::Date(_) => "java.time.LocalDate",
        Time::DateTime(_) => "java.time.LocalDateTime",
        Time::Instant(_) => "java.time.Instant",
        Time::Duration(_) => "java.time.Duration",
    };
    Value::Object(Rc::new(RefCell::new(Object {
        class,
        state: State::Time(time),
    })))
}

/// A static method of a `java.time` class, or `None` when there is no such method.
pub(super) fn time_static(
    class: &str,
    name: &str,
    arguments: &[Value],
) -> Option<Result<Value, Exception>> {
    let parse_error = |text: &str| {
        let message = format!("Text '{text}' could not be parsed");
        Exception::new("DateTimeParseException", message)
    };
    let int = |value: &Value| match value {
        Value::Number(number) => Some(number.to_i64()),
        _ => None,
    };
    let time = match (class, name, arguments) {
        ("LocalDate", "now", []) => Time::Date(Zoned::now().date()),
        ("LocalDateTime", "now", []) => Time::DateTime(Zoned::now().datetime()),
        ("Instant", "now", []) => Time::Instant(Timestamp::now()),
        ("LocalDate", "parse", [text]) => {
            let text = text.to_string();
            match text.parse::<Date>() {
                Ok(date) => Time::Date(date),
                Err(_) => return Some(Err(parse_error(&text))),
            }
        }
        ("LocalDateTime", "parse", [text]) => {
            let text = text.to_string();
            match text.contains('T').then(|| text.parse::<DateTime>()) {
                Some(Ok(time)) => Time::DateTime(time),
                _ => return Some(Err(parse_error(&text))),
            }
        }
        ("Instant", "parse", [text]) => {
            let text = text.to_string();
            match text.parse::<Timestamp>() {
                Ok(time) => Time::Instant(time),
                Err(_) => return Some(Err(parse_error(&text))),
            }
        }
        ("LocalDate", "of", [year, month, day]) => {
            let parts = (int(year)?, int(month)?, int(day)?);
            match Date::new(parts.0 as i16, parts.1 as i8, parts.2 as i8) {
                Ok(date) => Time::Date(date),
                Err(error) => {
                    return Some(Err(Exception::new("DateTimeException", error.to_string())));
                }
            }
        }
        ("LocalDateTime", "of", [year, month, day, hour, minute, rest @ ..]) if rest.len() <= 1 => {
            let second = match rest {
                [second] => int(second)?,
                _ => 0,
            };
            let (hour, minute) = (int(hour)? as i8, int(minute)? as i8);
            let date = Date::new(int(year)? as i16, int(month)? as i8, int(day)? as i8);
            let time = date.and_then(|date| {
                let time = jiff::civil::Time::new(hour, minute, second as i8, 0);
                time.map(|time| date.to_datetime(time))
            });
            match time {
                Ok(time) => Time::DateTime(time),
                Err(error) => {
                    return Some(Err(Exception::new("DateTimeException", error.to_string())));
                }
            }
        }
        ("Instant", "ofEpochMilli", [millis]) => {
            Time::Instant(Timestamp::from_millisecond(int(millis)?).ok()?)
        }
        ("Instant", "ofEpochSecond", [seconds]) => {
            Time::Instant(Timestamp::from_second(int(seconds)?).ok()?)
        }
        ("java.time.Duration", "ofMillis", [count]) => {
            Time::Duration(SignedDuration::from_millis(int(count)?))
        }
        ("java.time.Duration", "ofSeconds", [count]) => {
            Time::Duration(SignedDuration::from_secs(int(count)?))
        }
        ("java.time.Duration", "ofMinutes", [count]) => {
            Time::Duration(SignedDuration::from_mins(int(count)?))
        }
        ("java.time.Duration", "ofHours", [count]) => {
            Time::Duration(SignedDuration::from_hours(int(count)?))
        }
        ("java.time.Duration", "ofDays", [count]) => {
            Time::Duration(SignedDuration::from_hours(int(count)? * 24))
        }
        ("java.time.Duration", "between", [Value::Object(start), Value::Object(end)]) => {
            let instant = |object: &Rc<RefCell<Object>>| match &object.borrow().state {
                State::Time(Time::Instant(time)) => Some(time.as_duration()),
                State::Time(Time::DateTime(time)) => {
                    Some(time.to_zoned(TimeZone::UTC).ok()?.timestamp().as_duration())
                }
                _ => None,
            };
            Time::Duration(instant(end)? - instant(start)?)
        }
        _ => return None,
    };
    Some(Ok(time_value(time)))
}

/// A method of a `java.time` value, or `None` when it has no such method.
pub(super) fn time_method(
    time: Time,
    name: &str,
    arguments: &[Value],
) -> Option<Result<Value, Exception>> {
    let int = |value: &Value| match value {
        Value::Number(number) => Some(number.to_i64()),
        _ => None,
    };
    let integer = |value: i64| Some(Ok(Value::integer(value as i32)));
    let long = |value: i64| Some(Ok(Value::Number(Number::Long(value))));
    let other = |value: &Value| match value {
        Value::Object(object) => match &object.borrow().state {
            State::Time(other) => Some(*other),
            _ => None,
        },
        _ => None,
    };
    let civil = match time {
        Time::Date(date) => Some(date.to_datetime(jiff::civil::Time::midnight())),
        Time::DateTime(time) => Some(time),
        _ => None,
    };
    match (name, arguments) {
        ("toString", []) => return Some(Ok(Value::string(time.text()))),
        ("isBefore" | "isAfter" | "isEqual", [value]) => {
            let order = time.partial_cmp(&other(value)?)?;
            let result = match name {
                "isBefore" => order.is_lt(),
                "isAfter" => order.is_gt(),
                _ => order.is_eq(),
            };
            return Some(Ok(Value::Boolean(result)));
        }
        ("compareTo", [value]) => {
            let order = time.partial_cmp(&other(value)?)?;
            return integer(order as i64);
        }
        _ => {}
    }
    if let Some(civil) = civil {
        let shifted = |span: jiff::Span| {
            let shifted = civil.checked_add(span).ok()?;
            Some(Ok(time_value(match time {
                Time::Date(_) => Time::Date(shifted.date()),
                _ => Time::DateTime(shifted),
            })))
        };
        return match (name, arguments) {
            ("format", [Value::Object(formatter)]) => {
                let pattern = match &formatter.borrow().state {
                    State::Text(pattern) => pattern.clone(),
                    _ => return None,
                };
                let zoned = civil.to_zoned(TimeZone::UTC).ok()?;
                Some(crate::date::format_zoned(&zoned, &pattern).map(Value::string))
            }
            ("getYear", []) => integer(i64::from(civil.year())),
            ("getMonthValue", []) => integer(i64::from(civil.month())),
            ("getDayOfMonth", []) => integer(i64::from(civil.day())),
            ("getDayOfYear", []) => integer(i64::from(civil.day_of_year())),
            ("getHour", []) if matches!(time, Time::DateTime(_)) => {
                integer(i64::from(civil.hour()))
            }
            ("getMinute", []) if matches!(time, Time::DateTime(_)) => {
                integer(i64::from(civil.minute()))
            }
            ("getSecond", []) if matches!(time, Time::DateTime(_)) => {
                integer(i64::from(civil.second()))
            }
            ("toLocalDate", []) => Some(Ok(time_value(Time::Date(civil.date())))),
            ("plusDays" | "minusDays", [count]) => shifted(sign(name) * int(count)?.days()),
            ("plusWeeks" | "minusWeeks", [count]) => shifted(sign(name) * int(count)?.weeks()),
            ("plusMonths" | "minusMonths", [count]) => shifted(sign(name) * int(count)?.months()),
            ("plusYears" | "minusYears", [count]) => shifted(sign(name) * int(count)?.years()),
            ("plusHours" | "minusHours", [count]) if matches!(time, Time::DateTime(_)) => {
                shifted(sign(name) * int(count)?.hours())
            }
            ("plusMinutes" | "minusMinutes", [count]) if matches!(time, Time::DateTime(_)) => {
                shifted(sign(name) * int(count)?.minutes())
            }
            ("plusSeconds" | "minusSeconds", [count]) if matches!(time, Time::DateTime(_)) => {
                shifted(sign(name) * int(count)?.seconds())
            }
            _ => None,
        };
    }
    match (time, name, arguments) {
        (Time::Instant(instant), "toEpochMilli", []) => long(instant.as_millisecond()),
        (Time::Instant(instant), "getEpochSecond", []) => long(instant.as_second()),
        (
            Time::Instant(instant),
            "plusMillis" | "plusSeconds" | "minusMillis" | "minusSeconds",
            [count],
        ) => {
            let count = sign(name) * int(count)?;
            let duration = match name.ends_with("Millis") {
                true => SignedDuration::from_millis(count),
                false => SignedDuration::from_secs(count),
            };
            Some(Ok(time_value(Time::Instant(
                instant.checked_add(duration).ok()?,
            ))))
        }
        (Time::Duration(duration), "toMillis", []) => long(duration.as_millis() as i64),
        (Time::Duration(duration), "getSeconds" | "toSeconds", []) => long(duration.as_secs()),
        (Time::Duration(duration), "toMinutes", []) => long(duration.as_secs() / 60),
        (Time::Duration(duration), "toHours", []) => long(duration.as_secs() / 3600),
        (Time::Duration(duration), "toDays", []) => long(duration.as_secs() / 86400),
        (Time::Duration(duration), "plus" | "minus", [value]) => match other(value)? {
            Time::Duration(other) => {
                let total = match name {
                    "plus" => duration.checked_add(other)?,
                    _ => duration.checked_sub(other)?,
                };
                Some(Ok(time_value(Time::Duration(total))))
            }
            _ => None,
        },
        _ => None,
    }
}

/// 1 for a `plus` method, -1 for a `minus` method.
fn sign(name: &str) -> i64 {
    if name.starts_with("minus") { -1 } else { 1 }
}
