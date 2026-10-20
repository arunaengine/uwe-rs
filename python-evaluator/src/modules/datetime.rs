//! Stand-ins for `datetime` and `time`: points in time from the clock of the host, in local
//! time or in a fixed time zone, and durations.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;

use super::function;
use crate::error::Error;
use crate::interpreter::{Interpreter, Operator};
use crate::number::to_f64;
use crate::value::{Args, Native, Value};

/// The format of a point in time as Python writes it: microseconds only when there are some.
fn time_format(time: &jiff::Zoned, separator: char) -> &'static str {
    match (time.subsec_nanosecond() != 0, separator) {
        (true, 'T') => "%Y-%m-%dT%H:%M:%S%.6f",
        (true, _) => "%Y-%m-%d %H:%M:%S%.6f",
        (false, 'T') => "%Y-%m-%dT%H:%M:%S",
        (false, _) => "%Y-%m-%d %H:%M:%S",
    }
}

/// A point in time for `datetime` and `time`. A naive one, without `tzinfo`, is kept in local
/// time.
#[derive(Clone)]
struct DateTime {
    time: jiff::Zoned,
    date_only: bool,
    naive: bool,
}

impl DateTime {
    fn naive(time: jiff::Zoned, date_only: bool) -> Value {
        Value::native(DateTime {
            time,
            date_only,
            naive: true,
        })
    }

    /// The offset from UTC as Python writes it, like `+00:00`; empty for a naive time.
    fn offset_text(&self) -> String {
        if self.naive {
            return String::new();
        }
        let seconds = self.time.offset().seconds();
        let sign = if seconds < 0 { '-' } else { '+' };
        let seconds = seconds.abs();
        format!("{sign}{:02}:{:02}", seconds / 3600, seconds % 3600 / 60)
    }

    /// `datetime.fromisoformat(text)`: a date, optionally with a time after `T` or a space,
    /// and an offset like `Z` or `+02:00`, which makes the time aware.
    fn from_iso(text: &str, date_only: bool) -> Result<Value, Error> {
        let invalid = || Error::value_error(format!("Invalid isoformat string: '{text}'"));
        let (body, offset) = match text.strip_suffix('Z') {
            Some(body) => (body, Some(0)),
            None => {
                let time_start = text.find(['T', ' ']).map_or(text.len(), |at| at + 1);
                match text[time_start..].rfind(['+', '-']) {
                    Some(at) => {
                        let sign = if &text[time_start + at..time_start + at + 1] == "-" {
                            -1
                        } else {
                            1
                        };
                        let zone = &text[time_start + at + 1..];
                        let (hours, minutes) = zone.split_once(':').unwrap_or((zone, "0"));
                        let hours: i32 = hours.parse().map_err(|_| invalid())?;
                        let minutes: i32 = minutes.parse().map_err(|_| invalid())?;
                        (
                            &text[..time_start + at],
                            Some(sign * (hours * 3600 + minutes * 60)),
                        )
                    }
                    None => (text, None),
                }
            }
        };
        let civil: jiff::civil::DateTime = if body.len() == 10 {
            body.parse::<jiff::civil::Date>()
                .map_err(|_| invalid())?
                .to_datetime(jiff::civil::Time::midnight())
        } else {
            body.replace(' ', "T").parse().map_err(|_| invalid())?
        };
        let zone = match offset {
            Some(seconds) => jiff::tz::TimeZone::fixed(
                jiff::tz::Offset::from_seconds(seconds).map_err(|_| invalid())?,
            ),
            None => jiff::tz::TimeZone::system(),
        };
        let time = civil.to_zoned(zone).map_err(|_| invalid())?;
        Ok(Value::native(DateTime {
            time,
            date_only,
            naive: offset.is_none(),
        }))
    }
}

impl Native for DateTime {
    fn type_name(&self) -> &str {
        if self.date_only {
            "datetime.date"
        } else {
            "datetime.datetime"
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn binary(
        &self,
        _: &mut Interpreter,
        operator: Operator,
        other: &Value,
        reflected: bool,
    ) -> Result<Option<Value>, Error> {
        if let Some(delta) = other.as_native::<TimeDelta>() {
            let span = jiff::SignedDuration::from_micros(delta.0);
            let moved = match (operator, reflected) {
                (Operator::Add, _) => self.time.checked_add(span),
                (Operator::Sub, false) => self.time.checked_sub(span),
                _ => return Ok(None),
            }
            .map_err(|error| Error::new("OverflowError", error.to_string()))?;
            return Ok(Some(Value::native(DateTime {
                time: moved,
                ..self.clone()
            })));
        }
        if let Some(earlier) = other.as_native::<DateTime>() {
            let (first, second) = if reflected {
                (&earlier.time, &self.time)
            } else {
                (&self.time, &earlier.time)
            };
            let micros = first.timestamp().as_microsecond() - second.timestamp().as_microsecond();
            return Ok(match operator {
                Operator::Sub => Some(Value::native(TimeDelta(micros))),
                Operator::Eq => Some(Value::Bool(micros == 0)),
                Operator::NotEq => Some(Value::Bool(micros != 0)),
                Operator::Lt => Some(Value::Bool(micros < 0)),
                Operator::LtE => Some(Value::Bool(micros <= 0)),
                Operator::Gt => Some(Value::Bool(micros > 0)),
                Operator::GtE => Some(Value::Bool(micros >= 0)),
                _ => None,
            });
        }
        Ok(None)
    }

    fn to_str(&self, _: &mut Interpreter) -> Result<Option<String>, Error> {
        Ok(Some(if self.date_only {
            self.time.strftime("%Y-%m-%d").to_string()
        } else {
            let text = self.time.strftime(time_format(&self.time, ' ')).to_string();
            text + &self.offset_text()
        }))
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let this = self.clone();
        Ok(Some(match name {
            "year" => Value::Int(i64::from(this.time.year())),
            "month" => Value::Int(i64::from(this.time.month())),
            "day" => Value::Int(i64::from(this.time.day())),
            "hour" => Value::Int(i64::from(this.time.hour())),
            "minute" => Value::Int(i64::from(this.time.minute())),
            "second" => Value::Int(i64::from(this.time.second())),
            "microsecond" => Value::Int(i64::from(this.time.subsec_nanosecond() / 1000)),
            "tzinfo" => {
                if this.naive {
                    Value::None
                } else {
                    Value::native(TimeZone(this.time.offset().seconds()))
                }
            }
            "weekday" | "isoweekday" => {
                let day = i64::from(this.time.weekday().to_monday_zero_offset());
                let iso = name == "isoweekday";
                Value::builtin(name, move |_, _| {
                    Ok(Value::Int(if iso { day + 1 } else { day }))
                })
            }
            "astimezone" => Value::builtin("astimezone", move |_, mut args| {
                let zone = args.take(0, "tz").filter(|zone| !zone.is_none());
                let zone = zone_of(zone.as_ref());
                Ok(Value::native(DateTime {
                    time: this.time.with_time_zone(zone),
                    date_only: this.date_only,
                    naive: false,
                }))
            }),
            "replace" => Value::builtin("replace", move |_, mut args| {
                let mut civil = this.time.datetime();
                let invalid = |error: jiff::Error| Error::value_error(error.to_string());
                for (index, name) in [
                    "year",
                    "month",
                    "day",
                    "hour",
                    "minute",
                    "second",
                    "microsecond",
                ]
                .iter()
                .enumerate()
                {
                    let Some(value) = args.take(index, name) else {
                        continue;
                    };
                    let number = value
                        .as_int()
                        .ok_or_else(|| Error::type_error(format!("'{name}' must be an integer")))?;
                    let small = |range: &str| {
                        i8::try_from(number)
                            .map_err(|_| Error::value_error(format!("{name} {range}")))
                    };
                    civil =
                        match *name {
                            "year" => civil.with().year(
                                i16::try_from(number)
                                    .map_err(|_| Error::value_error("year is out of range"))?,
                            ),
                            "month" => civil.with().month(small("must be in 1..12")?),
                            "day" => civil.with().day(small("is out of range for month")?),
                            "hour" => civil.with().hour(small("must be in 0..23")?),
                            "minute" => civil.with().minute(small("must be in 0..59")?),
                            "second" => civil.with().second(small("must be in 0..59")?),
                            _ => civil.with().subsec_nanosecond(
                                i32::try_from(number * 1000).map_err(|_| {
                                    Error::value_error("microsecond must be in 0..999999")
                                })?,
                            ),
                        }
                        .build()
                        .map_err(invalid)?;
                }
                let (zone, naive) = match args.take(7, "tzinfo") {
                    Some(zone) if zone.is_none() => (jiff::tz::TimeZone::system(), true),
                    Some(zone) => (zone_of(Some(&zone)), false),
                    None => (this.time.time_zone().clone(), this.naive),
                };
                Ok(Value::native(DateTime {
                    time: civil.to_zoned(zone).map_err(invalid)?,
                    date_only: this.date_only,
                    naive,
                }))
            }),
            "strftime" => Value::builtin("strftime", move |_, args| {
                let format = args
                    .positional
                    .first()
                    .and_then(|format| format.as_str().map(str::to_owned))
                    .unwrap_or_default();
                Ok(Value::str(this.time.strftime(&format).to_string()))
            }),
            "isoformat" => Value::builtin("isoformat", move |_, _| {
                Ok(Value::str(if this.date_only {
                    this.time.strftime("%Y-%m-%d").to_string()
                } else {
                    let text = this.time.strftime(time_format(&this.time, 'T')).to_string();
                    text + &this.offset_text()
                }))
            }),
            "date" => Value::builtin("date", move |_, _| {
                Ok(Value::native(DateTime {
                    time: this.time.clone(),
                    date_only: true,
                    naive: true,
                }))
            }),
            "timestamp" => Value::builtin("timestamp", move |_, _| {
                Ok(Value::Float(this.time.timestamp().as_second() as f64))
            }),
            _ => return Ok(None),
        }))
    }
}

struct DateClass(bool);

impl Native for DateClass {
    fn type_name(&self) -> &str {
        "type"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn check_instance(&self, value: &Value) -> Option<bool> {
        Some(value.as_native::<DateTime>().is_some())
    }

    fn instance_of(&self, class: &str) -> bool {
        class == "callable"
    }

    /// `datetime(year, month, day, hour=0, minute=0, second=0, microsecond=0, tzinfo=None)`.
    fn call(&self, _: &mut Interpreter, _: &Value, mut args: Args) -> Result<Option<Value>, Error> {
        let mut parts = [0i64; 7];
        for (index, name) in [
            "year",
            "month",
            "day",
            "hour",
            "minute",
            "second",
            "microsecond",
        ]
        .iter()
        .enumerate()
        {
            parts[index] = match args.take(index, name) {
                Some(value) => value
                    .as_int()
                    .ok_or_else(|| Error::type_error(format!("'{name}' must be an integer")))?,
                None if index < 3 => {
                    return Err(Error::type_error(format!(
                        "function missing required argument '{name}'"
                    )));
                }
                None => 0,
            };
        }
        let tzinfo = args.take(7, "tzinfo").filter(|zone| !zone.is_none());
        let naive = tzinfo.is_none();
        let zone = zone_of(tzinfo.as_ref());
        let invalid = |error: jiff::Error| Error::value_error(error.to_string());
        let civil = jiff::civil::DateTime::new(
            i16::try_from(parts[0]).map_err(|_| Error::value_error("year is out of range"))?,
            i8::try_from(parts[1]).map_err(|_| Error::value_error("month must be in 1..12"))?,
            i8::try_from(parts[2])
                .map_err(|_| Error::value_error("day is out of range for month"))?,
            i8::try_from(parts[3]).map_err(|_| Error::value_error("hour must be in 0..23"))?,
            i8::try_from(parts[4]).map_err(|_| Error::value_error("minute must be in 0..59"))?,
            i8::try_from(parts[5]).map_err(|_| Error::value_error("second must be in 0..59"))?,
            i32::try_from(parts[6] * 1000)
                .map_err(|_| Error::value_error("microsecond must be in 0..999999"))?,
        )
        .map_err(invalid)?;
        let time = civil.to_zoned(zone).map_err(invalid)?;
        Ok(Some(Value::native(DateTime {
            time,
            naive,
            date_only: self.0,
        })))
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let date_only = self.0;
        Ok(match name {
            "now" | "today" | "utcnow" => {
                let utc = name == "utcnow";
                Some(Value::builtin(name, move |_, mut args| {
                    let zone = args.take(0, "tz").filter(|zone| !zone.is_none());
                    let naive = zone.is_none();
                    let zone = if utc {
                        jiff::tz::TimeZone::UTC
                    } else {
                        zone_of(zone.as_ref())
                    };
                    Ok(Value::native(DateTime {
                        time: jiff::Timestamp::now().to_zoned(zone),
                        date_only,
                        naive,
                    }))
                }))
            }
            "strptime" => Some(Value::builtin("strptime", move |interpreter, args| {
                let text = interpreter.to_str(args.get(0).unwrap_or(&Value::None))?;
                let format = interpreter.to_str(args.get(1).unwrap_or(&Value::None))?;
                let invalid = |_| {
                    Error::value_error(format!(
                        "time data {} does not match format {}",
                        crate::format::quote(&text),
                        crate::format::quote(&format)
                    ))
                };
                let parsed = jiff::fmt::strtime::parse(&format, &text).map_err(invalid)?;
                let civil = parsed
                    .to_datetime()
                    .or_else(|_| {
                        parsed
                            .to_date()
                            .map(|date| date.to_datetime(jiff::civil::Time::midnight()))
                    })
                    .map_err(invalid)?;
                let time = civil
                    .to_zoned(jiff::tz::TimeZone::system())
                    .map_err(|error| Error::value_error(error.to_string()))?;
                Ok(DateTime::naive(time, date_only))
            })),
            "fromtimestamp" => Some(Value::builtin("fromtimestamp", move |_, args| {
                let seconds = args.positional.first().and_then(to_f64).unwrap_or(0.0) as i64;
                let timestamp = jiff::Timestamp::from_second(seconds)
                    .map_err(|error| Error::value_error(error.to_string()))?;
                Ok(DateTime::naive(
                    timestamp.to_zoned(jiff::tz::TimeZone::system()),
                    date_only,
                ))
            })),
            "fromisoformat" => Some(Value::builtin("fromisoformat", move |interpreter, args| {
                let text = interpreter.to_str(args.get(0).unwrap_or(&Value::None))?;
                DateTime::from_iso(text.trim(), date_only)
            })),
            _ => None,
        })
    }
}

/// The time zone of a `tzinfo` argument: UTC or a fixed offset, else local time.
fn zone_of(value: Option<&Value>) -> jiff::tz::TimeZone {
    match value.and_then(|value| value.as_native::<TimeZone>()) {
        Some(zone) => jiff::tz::TimeZone::fixed(
            jiff::tz::Offset::from_seconds(zone.0).unwrap_or(jiff::tz::Offset::UTC),
        ),
        None => jiff::tz::TimeZone::system(),
    }
}

/// A duration, as `datetime.timedelta`, in whole microseconds.
#[derive(Clone, Copy)]
struct TimeDelta(i64);

impl TimeDelta {
    /// `timedelta(days=0, seconds=0, microseconds=0, milliseconds=0, minutes=0, hours=0,
    /// weeks=0)`.
    fn new(mut args: Args) -> Result<TimeDelta, Error> {
        let mut total = 0.0;
        for (index, (name, micros)) in [
            ("days", 86_400e6),
            ("seconds", 1e6),
            ("microseconds", 1.0),
            ("milliseconds", 1e3),
            ("minutes", 60e6),
            ("hours", 3_600e6),
            ("weeks", 604_800e6),
        ]
        .into_iter()
        .enumerate()
        {
            if let Some(value) = args.take(index, name) {
                let amount = to_f64(&value).ok_or_else(|| {
                    Error::type_error(format!("unsupported type for timedelta {name} component"))
                })?;
                total += amount * micros;
            }
        }
        Ok(TimeDelta(total.round() as i64))
    }

    fn days(self) -> i64 {
        self.0.div_euclid(86_400_000_000)
    }

    fn seconds(self) -> i64 {
        self.0.rem_euclid(86_400_000_000) / 1_000_000
    }
}

impl Native for TimeDelta {
    fn type_name(&self) -> &str {
        "datetime.timedelta"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let delta = *self;
        Ok(Some(match name {
            "days" => Value::Int(delta.days()),
            "seconds" => Value::Int(delta.seconds()),
            "microseconds" => Value::Int(delta.0.rem_euclid(1_000_000)),
            "total_seconds" => Value::builtin("total_seconds", move |_, _| {
                Ok(Value::Float(delta.0 as f64 / 1e6))
            }),
            _ => return Ok(None),
        }))
    }

    fn binary(
        &self,
        _: &mut Interpreter,
        operator: Operator,
        other: &Value,
        reflected: bool,
    ) -> Result<Option<Value>, Error> {
        if let Some(other) = other.as_native::<TimeDelta>() {
            let (a, b) = if reflected {
                (other.0, self.0)
            } else {
                (self.0, other.0)
            };
            return Ok(Some(match operator {
                Operator::Add => Value::native(TimeDelta(a + b)),
                Operator::Sub => Value::native(TimeDelta(a - b)),
                Operator::TrueDiv if b != 0 => Value::Float(a as f64 / b as f64),
                Operator::Eq => Value::Bool(a == b),
                Operator::NotEq => Value::Bool(a != b),
                Operator::Lt => Value::Bool(a < b),
                Operator::LtE => Value::Bool(a <= b),
                Operator::Gt => Value::Bool(a > b),
                Operator::GtE => Value::Bool(a >= b),
                _ => return Ok(None),
            }));
        }
        let Some(factor) = to_f64(other) else {
            return Ok(None);
        };
        Ok(match operator {
            Operator::Mul => Some(Value::native(TimeDelta(
                (self.0 as f64 * factor).round() as i64
            ))),
            Operator::TrueDiv if !reflected && factor != 0.0 => Some(Value::native(TimeDelta(
                (self.0 as f64 / factor).round() as i64,
            ))),
            _ => None,
        })
    }

    fn unary(&self, _: &mut Interpreter, operator: &str) -> Result<Option<Value>, Error> {
        Ok(match operator {
            "-" => Some(Value::native(TimeDelta(-self.0))),
            "+" => Some(Value::native(*self)),
            _ => None,
        })
    }

    fn to_str(&self, _: &mut Interpreter) -> Result<Option<String>, Error> {
        let (days, seconds) = (self.days(), self.seconds());
        let micros = self.0.rem_euclid(1_000_000);
        let mut text = String::new();
        if days != 0 {
            text.push_str(&format!(
                "{days} day{}, ",
                if days.abs() == 1 { "" } else { "s" }
            ));
        }
        text.push_str(&format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds % 3600 / 60,
            seconds % 60
        ));
        if micros != 0 {
            text.push_str(&format!(".{micros:06}"));
        }
        Ok(Some(text))
    }
}

/// A fixed time zone, as `datetime.timezone`, by its offset from UTC in seconds.
struct TimeZone(i32);

impl Native for TimeZone {
    fn type_name(&self) -> &str {
        "datetime.timezone"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `datetime.timezone`: callable with an offset, and with `timezone.utc`.
struct TimeZoneClass;

impl Native for TimeZoneClass {
    fn type_name(&self) -> &str {
        "type"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn check_instance(&self, value: &Value) -> Option<bool> {
        Some(value.as_native::<TimeZone>().is_some())
    }

    fn call(&self, _: &mut Interpreter, _: &Value, args: Args) -> Result<Option<Value>, Error> {
        let offset = args
            .get(0)
            .and_then(|value| value.as_native::<TimeDelta>())
            .map(|delta| delta.0 / 1_000_000)
            .ok_or_else(|| Error::type_error("timezone() argument 1 must be datetime.timedelta"))?;
        Ok(Some(Value::native(TimeZone(offset as i32))))
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        Ok(match name {
            "utc" => Some(Value::native(TimeZone(0))),
            _ => None,
        })
    }
}

pub fn datetime() -> Vec<(&'static str, Value)> {
    vec![
        ("datetime", Value::native(DateClass(false))),
        ("date", Value::native(DateClass(true))),
        (
            "timedelta",
            function("timedelta", |_, args| {
                Ok(Value::native(TimeDelta::new(args)?))
            }),
        ),
        ("timezone", Value::native(TimeZoneClass)),
        ("UTC", Value::native(TimeZone(0))),
    ]
}

pub fn time() -> Vec<(&'static str, Value)> {
    vec![
        (
            "time",
            function("time", |_, _| {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0.0, |duration| duration.as_secs_f64());
                Ok(Value::Float(now))
            }),
        ),
        (
            "perf_counter",
            function("perf_counter", |_, _| Ok(Value::Float(0.0))),
        ),
        (
            "monotonic",
            function("monotonic", |_, _| Ok(Value::Float(0.0))),
        ),
        ("sleep", function("sleep", |_, _| Ok(Value::None))),
        (
            "strftime",
            function("strftime", |_, args| {
                let format = args
                    .positional
                    .first()
                    .and_then(|format| format.as_str().map(str::to_owned))
                    .unwrap_or_default();
                // The time to format, as `localtime()` gives it; else now.
                let time = match args
                    .positional
                    .get(1)
                    .and_then(|time| time.as_native::<DateTime>())
                {
                    Some(time) => time.time.clone(),
                    None => jiff::Zoned::now(),
                };
                Ok(Value::str(time.strftime(&format).to_string()))
            }),
        ),
        (
            "localtime",
            function("localtime", |_, args| {
                Ok(DateTime::naive(
                    zoned_at(&args, jiff::tz::TimeZone::system())?,
                    false,
                ))
            }),
        ),
        (
            "gmtime",
            function("gmtime", |_, args| {
                Ok(DateTime::naive(
                    zoned_at(&args, jiff::tz::TimeZone::UTC)?,
                    false,
                ))
            }),
        ),
    ]
}

/// The time of seconds since the epoch in a zone, as `time.localtime(secs)` takes them; now
/// without seconds.
fn zoned_at(args: &Args, zone: jiff::tz::TimeZone) -> Result<jiff::Zoned, Error> {
    let seconds = match args.positional.first().map(Value::plain) {
        None | Some(Value::None) => return Ok(jiff::Timestamp::now().to_zoned(zone)),
        Some(Value::Int(seconds)) => *seconds as f64,
        Some(Value::Float(seconds)) => *seconds,
        Some(_) => return Err(Error::type_error("an integer or a float is required")),
    };
    let stamp = jiff::Timestamp::from_second(seconds.floor() as i64)
        .map_err(|error| Error::value_error(error.to_string()))?;
    Ok(stamp.to_zoned(zone))
}
