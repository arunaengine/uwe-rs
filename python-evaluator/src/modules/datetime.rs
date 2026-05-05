//! Stand-ins for `datetime` and `time`: points in time in local time, from the clock of the
//! host.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;

use super::function;
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::number::to_f64;
use crate::value::{Native, Value};

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

    fn to_str(&self, _: &mut Interpreter) -> Result<Option<String>, Error> {
        Ok(Some(if self.date_only {
            self.time.strftime("%Y-%m-%d").to_string()
        } else {
            self.time.strftime("%Y-%m-%d %H:%M:%S%.6f").to_string()
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
                    this.time.strftime("%Y-%m-%dT%H:%M:%S%.6f").to_string()
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

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let date_only = self.0;
        Ok(match name {
            "now" | "today" | "utcnow" => Some(Value::builtin(name, move |_, _| {
                Ok(Value::native(DateTime {
                    time: jiff::Zoned::now(),
                    date_only,
                }))
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

pub fn datetime() -> Vec<(&'static str, Value)> {
    vec![
        ("datetime", Value::native(DateClass(false))),
        ("date", Value::native(DateClass(true))),
        (
            "timedelta",
            function("timedelta", |_, _| {
                Err(Error::unsupported("datetime.timedelta"))
            }),
        ),
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
