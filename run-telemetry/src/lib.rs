//! OpenTelemetry traces of runs: a run of the run database as a trace, with the run as the
//! root span, each task as a span below it and each attempt below its task, with their
//! attributes, markers and events, in the OTLP JSON format. The traces go to a file of JSON
//! lines, one export request per line, as the file exporter of OpenTelemetry writes them and
//! the `otlpjsonfile` receiver of the OpenTelemetry Collector reads them, and to an OTLP HTTP
//! endpoint that the config file names. No OpenTelemetry library is needed: the format is
//! plain JSON.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod otlp;
mod send;

use std::io::Write;

pub use otlp::{KeyValue, Request, Span, SpanEvent, Status, Value};
pub use send::{Sent, TelemetryError, send, traces_address};

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
