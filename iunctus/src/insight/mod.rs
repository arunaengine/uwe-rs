//! The commands that show what runs did and why, from the run database: `iunctus time` tells
//! where the time of a run went, `iunctus compare` what differs between two runs, and
//! `iunctus replay` runs one task again and tells what differs, and `iunctus telemetry` writes
//! and sends the trace of a run in the OpenTelemetry format. `time` and `compare` write plain
//! text, or JSON with `-format json`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod compare;
pub mod replay;
pub mod telemetry;
pub mod time;

use run_database::{Database, Run};

use crate::arguments::Arguments;
use crate::{Failure, failure};

/// Whether `-format` asks for JSON rather than text, which is the default.
fn json_format(arguments: &Arguments, command: &str) -> Result<bool, Failure> {
    match arguments.format.as_deref() {
        None | Some("text") => Ok(false),
        Some("json") => Ok(true),
        Some(other) => Err(Failure::usage(format!(
            "iunctus: `iunctus {command}` writes text or json, not `{other}`"
        ))),
    }
}

/// Prints a value as JSON, from `serde_json::to_value`.
fn print_json(value: serde_json::Result<serde_json::Value>) -> Result<(), Failure> {
    let broken = |error: serde_json::Error| failure(error.to_string());
    let text = serde_json::to_string_pretty(&value.map_err(broken)?).map_err(broken)?;
    println!("{text}");
    Ok(())
}

/// A run of the database by its id, or a plain error that says how to find the runs.
fn run(database: &Database, id: &str) -> Result<Run, Failure> {
    database
        .run(id)
        .map_err(|error| failure(error.to_string()))?
        .ok_or_else(|| {
            failure(format!(
                "iunctus: the run database has no run `{id}`; `iunctus runs` lists the runs"
            ))
        })
}

/// A length of time in milliseconds, in words that fit its size, like `850 ms`, `4.2 s`,
/// `3 min 5 s` or `2 h 4 min`.
fn duration(milliseconds: i64) -> String {
    let milliseconds = milliseconds.max(0);
    if milliseconds < 1000 {
        return format!("{milliseconds} ms");
    }
    let seconds = milliseconds as f64 / 1000.0;
    if seconds < 60.0 {
        return format!("{seconds:.1} s");
    }
    let whole = milliseconds / 1000;
    let minutes = whole / 60;
    if minutes < 60 {
        return format!("{minutes} min {} s", whole % 60);
    }
    format!("{} h {} min", minutes / 60, minutes % 60)
}

/// A number of bytes with a unit of powers of 1000, like `12.3 MB`.
fn bytes(size: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = size.max(0) as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    match unit {
        0 => format!("{size} B"),
        _ => format!("{value:.1} {}", UNITS[unit]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_report_units() {
        assert_eq!(duration(850), "850 ms");
        assert_eq!(duration(4_200), "4.2 s");
        assert_eq!(duration(185_000), "3 min 5 s");
        assert_eq!(duration(7_440_000), "2 h 4 min");
        assert_eq!(duration(-3), "0 ms");
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(12_345_678), "12.3 MB");
    }
}
