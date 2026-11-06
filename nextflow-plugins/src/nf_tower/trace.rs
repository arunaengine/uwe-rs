//! Task records and the run graph as the trace API of Seqera Platform takes them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::observer::{Start, Task};
use crate::report_format::{Json, offset_time};
use crate::run_metadata::{TRACE_FIELDS, trace_value};

/// A task record with the Platform's field names: camel case, `%cpu` as `pcpu`, and times as
/// ISO text, followed by the fields of cloud and GPU runs, which uwe does not know.
pub(super) fn record(task: &Task) -> Json {
    let mut entries = Vec::new();
    for field in TRACE_FIELDS {
        let value = trace_value(task, field);
        let value = match field {
            "submit" | "start" | "complete" => {
                (value.as_i64()).map_or(Json::Null, |millis| Json::from(offset_time(millis)))
            }
            "tag" if !value.is_string() => Json::Null,
            _ => Json::from(&value),
        };
        entries.push((camel(field), value));
    }
    entries.push(("executor".into(), Json::from(&task.executor)));
    for field in [
        "cloudZone",
        "machineType",
        "priceModel",
        "numSpotInterruptions",
    ]
    .into_iter()
    .chain(["logStreamId", "resourceAllocation", "gpuMetrics"])
    {
        entries.push((field.into(), Json::Null));
    }
    Json::Object(entries)
}

/// A trace field name as the Platform names it, like `peakRss` for `peak_rss`.
fn camel(field: &str) -> String {
    let field = match field.strip_prefix('%') {
        Some(rest) => format!("p{rest}"),
        None => field.to_owned(),
    };
    let mut parts = field.split('_');
    let mut name = parts.next().unwrap_or_default().to_owned();
    for part in parts {
        let mut characters = part.chars();
        if let Some(first) = characters.next() {
            name.extend(first.to_uppercase());
            name.push_str(characters.as_str());
        }
    }
    name
}

/// The run graph as 1.29 sends it: uwe's graph has the processes and the processes that
/// their outputs reach, but no operators; left out when it has no process.
pub(super) fn dag(start: &Start) -> Json {
    if start.processes.is_empty() {
        return Json::Null;
    }
    let vertices = start.processes.iter().map(|process| {
        let scope = (process.name.rsplit_once(':')).map(|(scope, _)| scope.to_owned());
        Json::object([
            ("id", Json::from(process.id.to_string())),
            ("type", Json::from("PROCESS")),
            ("label", Json::from(&process.name)),
            ("scope", Json::from(scope)),
        ])
    });
    let edges = start.processes.iter().flat_map(|process| {
        process.feeds.iter().map(|target| {
            Json::object([
                ("source", Json::from(process.id.to_string())),
                ("target", Json::from(target.to_string())),
                ("label", Json::Null),
            ])
        })
    });
    Json::object([
        ("vertices", Json::List(vertices.collect())),
        ("edges", Json::List(edges.collect())),
    ])
}
