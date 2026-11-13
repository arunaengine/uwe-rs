//! nf-bactopia 2.1.7: Bactopia's input, validation and record helpers, banner and run summary.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod console;
mod inputs;
mod records;
mod tools;
mod validate;

use std::cell::RefCell;
use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::{Map, MapCell, MapKind, Value};

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};
use crate::nf_schema::get;

pub(crate) use console::{finish, start};

/// `functions.nf` gives the record helpers, which take lists and channels, over the native
/// helpers of `call`.
pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &[
        "bactopiaInputs",
        "bactopiaToolInputs",
        "validateParameters",
        "getCapturedLogs",
        "clearCapturedLogs",
        "gather",
        "gatherCsvtk",
        "gatherFields",
        "filterWithData",
        "collectNextflowLogs",
        "combineWith",
        "formatSamples",
    ],
    call: Some(call),
    script: Some(include_str!("functions.nf")),
    scopes: &["bactopia"],
    ..Plugin::new("nf-bactopia", Kind::Functions, State::Implemented)
};

fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    let text = |value: &Value| value.to_string();
    match (name, arguments.as_slice()) {
        ("bactopiaInputs", [runtype]) => {
            let samples = inputs::bactopia(&text(runtype), context)?;
            Ok(result(&[("samples", samples)], false))
        }
        ("bactopiaToolInputs", []) => {
            let samples = tools::samples(context)?;
            Ok(result(&[("samples", samples)], false))
        }
        ("validateParameters", [tool]) => {
            let runtype = validate::parameters(tool.truth(), context)?;
            Ok(result(&[("data", Value::string(runtype))], true))
        }
        ("validateParameters", []) => {
            let runtype = validate::parameters(false, context)?;
            Ok(result(&[("data", Value::string(runtype))], true))
        }
        ("getCapturedLogs", []) => Ok(Value::string(take_logs(true))),
        ("getCapturedLogs", [colors]) => Ok(Value::string(take_logs(colors.truth()))),
        ("clearCapturedLogs", []) => {
            take_logs(false);
            Ok(Value::Null)
        }
        (name, arguments) => records::call(name, arguments),
    }
}

/// The level of a captured log message.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Level {
    Error,
    Warn,
    Info,
}

thread_local! {
    /// The messages the plugin logged since the last result took them, like its log appender.
    static CAPTURED: RefCell<Vec<(Level, String)>> = const { RefCell::new(Vec::new()) };
}

/// Logs a message and keeps it for the next result.
fn log(level: Level, message: impl Into<String>) {
    let message = message.into();
    match level {
        Level::Error => tracing::error!("{message}"),
        Level::Warn => tracing::warn!("{message}"),
        Level::Info => tracing::info!("{message}"),
    }
    CAPTURED.with(|captured| captured.borrow_mut().push((level, message)));
}

/// The captured messages, one per line, errors in red and warnings in yellow; then clears them.
fn take_logs(colors: bool) -> String {
    let messages = CAPTURED.with(|captured| std::mem::take(&mut *captured.borrow_mut()));
    let lines: Vec<String> = (messages.into_iter())
        .map(|(level, message)| {
            let color = match level {
                Level::Error => "\u{1b}[0;31m",
                Level::Warn => "\u{1b}[0;33m",
                Level::Info => "",
            };
            match colors && !color.is_empty() && !message.contains("\u{1b}[") {
                true => format!("{color}{message}\u{1b}[0m"),
                false => message,
            }
        })
        .collect();
    lines.join("\n")
}

/// The map that the input and validation functions return, built from the captured log.
fn result(fields: &[(&str, Value)], validation: bool) -> Value {
    let errors = CAPTURED
        .with(|captured| (captured.borrow().iter()).any(|(level, _)| *level == Level::Error));
    let logs = take_logs(true);
    let error = match errors {
        true => format!("\u{1b}[0;31m!! ERROR !!\u{1b}[0m\n\n{logs}"),
        false => String::new(),
    };
    let text = |text: &str| Value::string(text);
    let mut entries = Vec::new();
    if validation {
        entries.extend(fields.iter().map(|(key, value)| (text(key), value.clone())));
        entries.push((text("logs"), text(&logs)));
        entries.push((text("hasErrors"), Value::Boolean(errors)));
        entries.push((text("error"), text(&error)));
    } else {
        entries.push((text("hasErrors"), Value::Boolean(errors)));
        entries.push((text("error"), text(&error)));
        entries.push((text("logs"), text(&logs)));
        entries.extend(fields.iter().map(|(key, value)| (text(key), value.clone())));
    }
    Value::map(entries)
}

/// A Nextflow 26 record, a `RecordMap`, of the entries in order.
fn record(entries: Vec<(&str, Value)>) -> Value {
    let map = Value::map(
        entries
            .into_iter()
            .map(|(key, value)| (Value::string(key), value)),
    );
    if let Value::Map(cell) = &map {
        cell.borrow_mut().kind = MapKind::Record;
    }
    map
}

/// A copy of a map as a record.
fn as_record(map: &Value) -> Value {
    match map {
        Value::Map(cell) => {
            let entries = cell.borrow().entries.clone();
            Value::Map(Rc::new(MapCell::new(Map {
                kind: MapKind::Record,
                entries,
                default: None,
            })))
        }
        other => other.clone(),
    }
}

/// The `IllegalArgumentException` that the plugin's functions throw.
fn argument(message: impl Into<String>) -> Error {
    Error::Thrown {
        exception: Rc::new(Exception::illegal_argument(message)),
        span: Default::default(),
    }
}

/// A param as text, when it is set: not null, not empty and not `false`.
fn param(context: &Context, name: &str) -> Option<String> {
    let value = get(&context.params, name)?;
    match value {
        Value::Null | Value::Boolean(false) => None,
        other => Some(other.to_string()).filter(|text| !text.is_empty()),
    }
}
