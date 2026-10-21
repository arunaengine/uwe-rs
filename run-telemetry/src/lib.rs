//! Exports recorded runs as OTLP JSON traces to files and HTTP collectors.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod live;
mod otlp;
mod send;
mod spans;

use std::io::Write;

pub use live::Live;
pub use otlp::{KeyValue, Request, Span, SpanEvent, Status, Value};
pub use send::{Sent, TelemetryError, send, traces_address};
pub use spans::requests;

/// How many spans one export request holds, so that a request of a large run stays small
/// enough for collectors, which take a few megabytes each by default.
pub const SPANS_PER_REQUEST: usize = 1000;

/// Writes the requests as JSON lines, one request per line.
pub fn write_lines(requests: &[Request], mut writer: impl Write) -> Result<(), TelemetryError> {
    for request in requests {
        serde_json::to_writer(&mut writer, request)?;
        writer.write_all(b"\n")?;
    }
    Ok(())
}
