//! R's typed missing values (`NA` of logical, integer, double and character) as script values,
//! and the helper functions of nf-r-ipc for missing values, dates and durations.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::{Native, Object, State as ObjectState, Time, Value};

use crate::nf_sqldb::thrown;

/// The type of an R `NA`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Logical,
    Integer,
    Double,
    Character,
}

impl Kind {
    /// The tag of the value graph, like `na_double`.
    pub(crate) fn tag(self) -> &'static str {
        match self {
            Kind::Logical => "na_logical",
            Kind::Integer => "na_integer",
            Kind::Double => "na_double",
            Kind::Character => "na_character",
        }
    }

    pub(crate) fn from_tag(tag: &str) -> Option<Kind> {
        [Kind::Logical, Kind::Integer, Kind::Double, Kind::Character]
            .into_iter()
            .find(|kind| kind.tag() == tag)
    }

    /// The R type name, like `double`.
    fn name(self) -> &'static str {
        &self.tag()[3..]
    }
}

/// An `NA` value, which prints as its kind, like `DOUBLE`.
#[derive(Debug)]
struct Missing(Kind);

impl Native for Missing {
    fn plugin(&self) -> &'static str {
        "nf-r-ipc"
    }

    fn text(&self) -> String {
        self.0.name().to_uppercase()
    }

    fn key(&self) -> Option<String> {
        Some(self.text())
    }
}

/// The `NA` value of a kind.
pub(crate) fn value(kind: Kind) -> Value {
    crate::object("NAValue", Missing(kind))
}

/// The kind of an `NA` value, `None` for any other value.
pub(crate) fn kind(value: &Value) -> Option<Kind> {
    crate::native::<Missing>(value).map(|missing| missing.0)
}

/// A `java.time.ZonedDateTime`, kept as its text.
#[derive(Debug)]
struct Zoned(String);

impl Native for Zoned {
    fn plugin(&self) -> &'static str {
        "nf-r-ipc"
    }

    fn text(&self) -> String {
        self.0.clone()
    }

    fn key(&self) -> Option<String> {
        Some(self.0.clone())
    }
}

/// The helpers that work on any values, by name; `None` for another name.
pub(crate) fn helper(name: &str, arguments: &[Value]) -> Option<Result<Value, Error>> {
    let first = arguments.first().cloned().unwrap_or(Value::Null);
    let second = arguments.get(1).cloned().unwrap_or(Value::Null);
    let na = kind(&first);
    let null = matches!(first, Value::Null);
    let is = |wanted: Kind| Ok(Value::Boolean(na == Some(wanted)));
    Some(match name {
        "isNULL" => Ok(Value::Boolean(null)),
        "isNA" => Ok(Value::Boolean(na.is_some())),
        "isNALogical" => is(Kind::Logical),
        "isNAInteger" => is(Kind::Integer),
        "isNADouble" => is(Kind::Double),
        "isNACharacter" => is(Kind::Character),
        "isMissing" => Ok(Value::Boolean(null || na.is_some())),
        "naType" => Ok(na.map_or(Value::Null, |kind| Value::string(kind.name()))),
        "coalesce" => Ok(if null || na.is_some() { second } else { first }),
        "coalesceNULL" => Ok(if null { second } else { first }),
        "coalesceNA" => Ok(if na.is_some() { second } else { first }),
        "renderValue" => Ok(Value::string(render(&first))),
        "assertNotMissing" => match null || na.is_some() {
            false => Ok(first),
            true => {
                let label = match &second {
                    Value::Null => "value".to_owned(),
                    other => other.to_string(),
                };
                Err(thrown(format!(
                    "Missing value for {label}: {}",
                    render(&first)
                )))
            }
        },
        "asLocalDate" => local_date(&first),
        "asInstantUtc" => instant(&first).map(|instant| match instant {
            Some(instant) => time(Time::Instant(instant)),
            None => Value::Null,
        }),
        "asZonedDateTime" => zoned(&first, &second),
        "asDurationSeconds" => seconds(&first),
        _ => return None,
    })
}

/// `NULL`, `NA<kind>`, or the value's text.
fn render(value: &Value) -> String {
    match (value, kind(value)) {
        (Value::Null, _) => "NULL".to_owned(),
        (_, Some(kind)) => format!("NA<{}>", kind.name()),
        (other, None) => other.to_string(),
    }
}

fn time(time: Time) -> Value {
    let class = match time {
        Time::Date(_) => "java.time.LocalDate",
        Time::Instant(_) => "java.time.Instant",
        _ => "java.time.Duration",
    };
    Value::Object(Rc::new(RefCell::new(Object {
        class,
        state: ObjectState::Time(time),
    })))
}

fn local_date(value: &Value) -> Result<Value, Error> {
    if matches!(value, Value::Null) {
        return Ok(Value::Null);
    }
    let text = value.to_string();
    match text.trim().parse::<Date>() {
        Ok(date) if text.trim().len() == 10 => Ok(time(Time::Date(date))),
        _ => Err(thrown(format!(
            "Failed to parse LocalDate from value: {text}"
        ))),
    }
}

/// An instant of ISO text like `2024-01-02T03:04:05Z`, or of R's `2024-01-02 03:04:05 UTC`.
fn instant(value: &Value) -> Result<Option<Timestamp>, Error> {
    if matches!(value, Value::Null) {
        return Ok(None);
    }
    let text = value.to_string();
    let trimmed = text.trim();
    let parsed = match trimmed.strip_suffix(" UTC") {
        Some(local) => (local.replace(' ', "T").parse::<DateTime>())
            .ok()
            .and_then(|local| local.to_zoned(TimeZone::UTC).ok())
            .map(|zoned| zoned.timestamp()),
        None => trimmed.parse::<Timestamp>().ok(),
    };
    parsed
        .map(Some)
        .ok_or_else(|| thrown(format!("Failed to parse UTC timestamp from value: {text}")))
}

/// The instant in a zone, UTC by default, as `ZonedDateTime` prints it, like
/// `2024-01-02T04:04:05+01:00[Europe/Paris]`.
fn zoned(value: &Value, zone: &Value) -> Result<Value, Error> {
    let Some(instant) = instant(value)? else {
        return Ok(Value::Null);
    };
    let name = match zone {
        Value::Null => "UTC".to_owned(),
        other => other.to_string(),
    };
    let found = TimeZone::get(&name).map_err(|_| thrown(format!("Invalid timezone id: {name}")))?;
    let local = instant.to_zoned(found);
    let mut text = Time::DateTime(local.datetime()).text();
    let offset = local.offset().seconds();
    match offset {
        0 => text.push('Z'),
        _ => {
            let sign = if offset < 0 { '-' } else { '+' };
            let (hours, minutes) = (offset.abs() / 3600, offset.abs() % 3600 / 60);
            text.push_str(&format!("{sign}{hours:02}:{minutes:02}"));
        }
    }
    text.push_str(&format!("[{name}]"));
    Ok(crate::object("java.time.ZonedDateTime", Zoned(text)))
}

/// A duration of a number of seconds, rounded to milliseconds.
fn seconds(value: &Value) -> Result<Value, Error> {
    let text = value.to_string();
    let number: f64 = match value {
        Value::Number(number) => number.to_f64(),
        other => other.to_string().trim().parse().map_err(|_| {
            thrown(format!(
                "Failed to parse duration seconds from value: {text}"
            ))
        })?,
    };
    if !number.is_finite() {
        return Err(thrown(format!(
            "Failed to parse duration seconds from value: {text}"
        )));
    }
    let millis = (number * 1000.0).round() as i64;
    Ok(time(Time::Duration(SignedDuration::from_millis(millis))))
}
