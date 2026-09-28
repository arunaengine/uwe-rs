//! The shapes of the OTLP JSON format of traces: an export request holds the spans of one
//! resource, iunctus, in one scope. Following the JSON form of the OTLP protocol, ids are hex
//! text, 64-bit numbers and times are decimal text, and enums are numbers.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde::Serialize;

/// The kind of a span that is neither a client, a server nor a message: `SPAN_KIND_INTERNAL`.
pub(crate) const INTERNAL: u8 = 1;

/// The codes of the status of a span.
pub(crate) const UNSET: u8 = 0;
pub(crate) const OK: u8 = 1;
pub(crate) const ERROR: u8 = 2;

/// One export request, as a line of a file of JSON lines or the body of `POST /v1/traces`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub resource_spans: Vec<ResourceSpans>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSpans {
    pub resource: Resource,
    pub scope_spans: Vec<ScopeSpans>,
}

/// What made the spans: iunctus, as `service.name`, with its version.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Resource {
    pub attributes: Vec<KeyValue>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ScopeSpans {
    pub scope: Scope,
    pub spans: Vec<Span>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Scope {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Span {
    pub trace_id: String,
    pub span_id: String,
    /// Empty for the root span.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub parent_span_id: String,
    pub name: String,
    pub kind: u8,
    pub start_time_unix_nano: String,
    pub end_time_unix_nano: String,
    pub attributes: Vec<KeyValue>,
    pub events: Vec<SpanEvent>,
    pub status: Status,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpanEvent {
    pub time_unix_nano: String,
    pub name: String,
    pub attributes: Vec<KeyValue>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Status {
    pub code: u8,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub message: String,
}

/// An attribute: a name and a value.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct KeyValue {
    pub key: String,
    pub value: Value,
}

/// The value of an attribute, written as `{"stringValue": ...}` and the like.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum Value {
    #[serde(rename = "stringValue")]
    Text(String),
    /// A 64-bit number, which the JSON form writes as decimal text.
    #[serde(rename = "intValue")]
    Integer(String),
    #[serde(rename = "boolValue")]
    Flag(bool),
}

impl KeyValue {
    pub(crate) fn text(key: &str, value: impl Into<String>) -> KeyValue {
        KeyValue {
            key: key.to_owned(),
            value: Value::Text(value.into()),
        }
    }

    pub(crate) fn integer(key: &str, value: i64) -> KeyValue {
        KeyValue {
            key: key.to_owned(),
            value: Value::Integer(value.to_string()),
        }
    }

    pub(crate) fn flag(key: &str, value: bool) -> KeyValue {
        KeyValue {
            key: key.to_owned(),
            value: Value::Flag(value),
        }
    }
}

impl Request {
    /// The spans of the request.
    pub fn spans(&self) -> impl Iterator<Item = &Span> {
        (self.resource_spans.iter())
            .flat_map(|resource| &resource.scope_spans)
            .flat_map(|scope| &scope.spans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_otlp_values() {
        let values = [
            KeyValue::text("a", "x"),
            KeyValue::integer("b", 5),
            KeyValue::flag("c", true),
        ];
        let json = serde_json::to_string(&values).expect("JSON");
        assert_eq!(
            json,
            r#"[{"key":"a","value":{"stringValue":"x"}},{"key":"b","value":{"intValue":"5"}},{"key":"c","value":{"boolValue":true}}]"#
        );
        let status = Status {
            code: OK,
            message: String::new(),
        };
        assert_eq!(
            serde_json::to_string(&status).expect("JSON"),
            r#"{"code":1}"#
        );
    }
}
