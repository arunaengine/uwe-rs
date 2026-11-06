//! How report plugins write their files: JSON with keys in order, laid out as Groovy's
//! `JsonOutput` does, and times as Java prints them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt::Write as _;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::Value;

/// The version of Nextflow that uwe runs pipelines as, which reports name as their engine.
pub(crate) const NEXTFLOW_VERSION: &str = "26.04.6";

/// A JSON value whose object keys keep the order they were added in, as report files need.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    Text(String),
    List(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub(crate) fn object<K: Into<String>>(entries: impl IntoIterator<Item = (K, Json)>) -> Json {
        Json::Object(
            (entries.into_iter())
                .map(|(key, value)| (key.into(), value))
                .collect(),
        )
    }

    pub(crate) fn list<T: Into<Json>>(items: impl IntoIterator<Item = T>) -> Json {
        Json::List(items.into_iter().map(Into::into).collect())
    }

    /// Adds an entry to an object, or replaces the entry of the same key.
    pub(crate) fn insert(&mut self, key: &str, value: Json) {
        if let Json::Object(entries) = self {
            match entries.iter_mut().find(|(name, _)| name == key) {
                Some(entry) => entry.1 = value,
                None => entries.push((key.to_owned(), value)),
            }
        }
    }

    pub(crate) fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(entries) => (entries.iter())
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    /// The value without the object entries whose value is null, at every depth.
    pub(crate) fn without_nulls(self) -> Json {
        match self {
            Json::Object(entries) => Json::Object(
                (entries.into_iter())
                    .filter(|(_, value)| *value != Json::Null)
                    .map(|(key, value)| (key, value.without_nulls()))
                    .collect(),
            ),
            Json::List(items) => Json::List(items.into_iter().map(Json::without_nulls).collect()),
            other => other,
        }
    }

    /// The text that `JsonOutput.prettyPrint` gives: four spaces a level, and an empty list or
    /// object as its brackets around a line that holds only the indent.
    pub(crate) fn pretty(&self) -> String {
        let mut text = String::new();
        self.write_pretty(&mut text, 0);
        text
    }

    /// The text that `JsonOutput.toJson` gives, without spaces.
    pub(crate) fn compact(&self) -> String {
        let mut text = String::new();
        self.write_compact(&mut text);
        text
    }

    fn write_pretty(&self, text: &mut String, level: usize) {
        let indent = |level: usize| "    ".repeat(level);
        match self {
            Json::List(items) => {
                text.push_str("[\n");
                if items.is_empty() {
                    text.push_str(&indent(level + 1));
                    text.push('\n');
                }
                for (index, item) in items.iter().enumerate() {
                    text.push_str(&indent(level + 1));
                    item.write_pretty(text, level + 1);
                    text.push_str(if index + 1 < items.len() { ",\n" } else { "\n" });
                }
                text.push_str(&indent(level));
                text.push(']');
            }
            Json::Object(entries) => {
                text.push_str("{\n");
                if entries.is_empty() {
                    text.push_str(&indent(level + 1));
                    text.push('\n');
                }
                for (index, (key, value)) in entries.iter().enumerate() {
                    text.push_str(&indent(level + 1));
                    quote(text, key);
                    text.push_str(": ");
                    value.write_pretty(text, level + 1);
                    text.push_str(if index + 1 < entries.len() {
                        ",\n"
                    } else {
                        "\n"
                    });
                }
                text.push_str(&indent(level));
                text.push('}');
            }
            scalar => scalar.write_compact(text),
        }
    }

    fn write_compact(&self, text: &mut String) {
        match self {
            Json::Null => text.push_str("null"),
            Json::Bool(value) => text.push_str(if *value { "true" } else { "false" }),
            Json::Number(number) => text.push_str(&number.to_string()),
            Json::Text(value) => quote(text, value),
            Json::List(items) => {
                text.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        text.push(',');
                    }
                    item.write_compact(text);
                }
                text.push(']');
            }
            Json::Object(entries) => {
                text.push('{');
                for (index, (key, value)) in entries.iter().enumerate() {
                    if index > 0 {
                        text.push(',');
                    }
                    quote(text, key);
                    text.push(':');
                    value.write_compact(text);
                }
                text.push('}');
            }
        }
    }
}

/// A JSON string as Groovy writes it: characters outside ASCII as `\uXXXX`.
fn quote(text: &mut String, value: &str) {
    text.push('"');
    for character in value.chars() {
        match character {
            '"' => text.push_str("\\\""),
            '\\' => text.push_str("\\\\"),
            '\n' => text.push_str("\\n"),
            '\r' => text.push_str("\\r"),
            '\t' => text.push_str("\\t"),
            '\u{8}' => text.push_str("\\b"),
            '\u{c}' => text.push_str("\\f"),
            ' '..='~' => text.push(character),
            other => {
                let mut units = [0; 2];
                for unit in other.encode_utf16(&mut units) {
                    let _ = write!(text, "\\u{unit:04x}");
                }
            }
        }
    }
    text.push('"');
}

impl From<&str> for Json {
    fn from(value: &str) -> Json {
        Json::Text(value.to_owned())
    }
}

impl From<String> for Json {
    fn from(value: String) -> Json {
        Json::Text(value)
    }
}

impl From<&String> for Json {
    fn from(value: &String) -> Json {
        Json::Text(value.clone())
    }
}

impl From<bool> for Json {
    fn from(value: bool) -> Json {
        Json::Bool(value)
    }
}

impl From<i64> for Json {
    fn from(value: i64) -> Json {
        Json::Number(value.into())
    }
}

impl From<u64> for Json {
    fn from(value: u64) -> Json {
        Json::Number(value.into())
    }
}

impl From<f64> for Json {
    fn from(value: f64) -> Json {
        serde_json::Number::from_f64(value).map_or(Json::Null, Json::Number)
    }
}

impl<T: Into<Json>> From<Option<T>> for Json {
    fn from(value: Option<T>) -> Json {
        value.map_or(Json::Null, Into::into)
    }
}

impl From<&serde_json::Value> for Json {
    fn from(value: &serde_json::Value) -> Json {
        match value {
            serde_json::Value::Null => Json::Null,
            serde_json::Value::Bool(value) => Json::Bool(*value),
            serde_json::Value::Number(number) => Json::Number(number.clone()),
            serde_json::Value::String(text) => Json::Text(text.clone()),
            serde_json::Value::Array(items) => Json::List(items.iter().map(Json::from).collect()),
            serde_json::Value::Object(entries) => Json::Object(
                (entries.iter())
                    .map(|(key, value)| (key.clone(), Json::from(value)))
                    .collect(),
            ),
        }
    }
}

/// A script value as JSON: paths as their text, durations and memory as their text.
pub(crate) fn value_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Boolean(value) => (*value).into(),
        Value::Number(Number::Integer(value)) => (*value).into(),
        Value::Number(Number::Long(value)) => (*value).into(),
        Value::Number(number) => serde_json::Number::from_f64(number.to_f64())
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::String(text) | Value::GString(text) => text.as_ref().into(),
        Value::List(list) => list.borrow().items.iter().map(value_json).collect(),
        Value::Map(map) => serde_json::Value::Object(
            (map.borrow().entries.iter())
                .map(|(key, value)| (key.0.to_string(), value_json(value)))
                .collect(),
        ),
        Value::Path(path) => match path.prefix {
            None => path.path.as_str().into(),
            Some(_) => path.uri().into(),
        },
        Value::MemoryUnit(_) | Value::Duration(_) => value.to_string().into(),
        _ => serde_json::Value::Null,
    }
}

/// A time as Java's `OffsetDateTime` prints it in the time zone of this machine, like
/// `2026-11-06T10:22:01.778+01:00`; seconds and milliseconds are left out when zero.
pub(crate) fn offset_time(millis: i64) -> String {
    let Ok(time) = Timestamp::from_millisecond(millis) else {
        return String::new();
    };
    let time = time.to_zoned(TimeZone::system());
    let mut text = time.strftime("%Y-%m-%dT%H:%M").to_string();
    let (second, milli) = (time.second(), millis.rem_euclid(1000));
    if second != 0 || milli != 0 {
        let _ = write!(text, ":{second:02}");
    }
    if milli != 0 {
        let _ = write!(text, ".{milli:03}");
    }
    match time.offset().seconds() {
        0 => text.push('Z'),
        _ => text.push_str(&time.strftime("%:z").to_string()),
    }
    text
}

/// A time in whole seconds in the time zone of this machine, like `2026-11-06T10:22:01+01:00`.
pub(crate) fn second_time(millis: i64) -> String {
    match Timestamp::from_millisecond(millis) {
        Ok(time) => (time.to_zoned(TimeZone::system()))
            .strftime("%Y-%m-%dT%H:%M:%S%:z")
            .to_string(),
        Err(_) => String::new(),
    }
}

/// The date of a time in the time zone of this machine, like `2026-11-06`.
pub(crate) fn local_date(millis: i64) -> String {
    match Timestamp::from_millisecond(millis) {
        Ok(time) => (time.to_zoned(TimeZone::system()))
            .strftime("%Y-%m-%d")
            .to_string(),
        Err(_) => String::new(),
    }
}

/// Milliseconds since 1970, now.
pub(crate) fn now() -> i64 {
    Timestamp::now().as_millisecond()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prints_like_groovy() {
        let value = Json::object([
            ("name", Json::from("a\u{2082}")),
            ("empty", Json::list(Vec::<Json>::new())),
            ("items", Json::list([1_i64, 2])),
            ("none", Json::object(Vec::<(String, Json)>::new())),
        ]);
        let expected = "{\n    \"name\": \"a\\u2082\",\n    \"empty\": [\n        \n    ],\n    \
                        \"items\": [\n        1,\n        2\n    ],\n    \"none\": {\n        \n    \
                        }\n}";
        assert_eq!(value.pretty(), expected);
        assert_eq!(
            value.compact(),
            "{\"name\":\"a\\u2082\",\"empty\":[],\"items\":[1,2],\"none\":{}}"
        );
    }
}
