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

/// A point in time for `datetime` and `time`, in local time.
#[derive(Clone)]
struct DateTime {
    time: jiff::Zoned,
    date_only: bool,
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
                date_only: self.date_only,
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
            self.time.strftime(time_format(&self.time, ' ')).to_string()
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
                    this.time.strftime(time_format(&this.time, 'T')).to_string()
                }))
            }),
            "date" => Value::builtin("date", move |_, _| {
                Ok(Value::native(DateTime {
                    time: this.time.clone(),
                    date_only: true,
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
        let zone = zone_of(args.take(7, "tzinfo").as_ref());
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
                    let zone = args.take(0, "tz");
                    let zone = if utc {
                        jiff::tz::TimeZone::UTC
                    } else {
                        zone_of(zone.as_ref())
                    };
                    Ok(Value::native(DateTime {
                        time: jiff::Timestamp::now().to_zoned(zone),
                        date_only,
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
                Ok(Value::native(DateTime { time, date_only }))
            })),
            "fromtimestamp" => Some(Value::builtin("fromtimestamp", move |_, args| {
                let seconds = args.positional.first().and_then(to_f64).unwrap_or(0.0) as i64;
                let timestamp = jiff::Timestamp::from_second(seconds)
                    .map_err(|error| Error::value_error(error.to_string()))?;
                Ok(Value::native(DateTime {
                    time: timestamp.to_zoned(jiff::tz::TimeZone::system()),
                    date_only,
                }))
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
                Ok(Value::str(jiff::Zoned::now().strftime(&format).to_string()))
            }),
        ),
        (
            "localtime",
            function("localtime", |_, _| {
                Ok(Value::native(DateTime {
                    time: jiff::Zoned::now(),
                    date_only: false,
                }))
            }),
        ),
    ]
}
