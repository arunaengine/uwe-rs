//! Groovy callbacks of compiled ops: the closures of the script by their source location, and
//! small closures that the compiler writes, like the case tests of a `branch`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_runtime::export::{Callback, Criteria};
use serde_json::{Map, Value as Json, json};

use crate::Sources;

/// A closure of the script: its code and the bytes of it in its source, which the Groovy
/// bridge finds it by, with its captured values by variable name.
pub(crate) fn closure(callback: &Callback, sources: &mut Sources) -> Json {
    let source = sources.script(&callback.script);
    let mut closure = json!({
        "language": "groovy",
        "code": callback.code,
        "scope": {"source": source, "start": callback.start, "end": callback.end},
    });
    if !callback.captures.is_empty() {
        let captures: Map<String, Json> = (callback.captures.iter())
            .map(|(name, value)| (name.clone(), json!({"value": nested(value, sources)})))
            .collect();
        closure["captures"] = Json::Object(captures);
    }
    closure
}

/// A captured value, whose captured closures name their scripts by source id.
pub(crate) fn nested(value: &Json, sources: &mut Sources) -> Json {
    let mut value = value.clone();
    if let Some(object) = value.as_object_mut()
        && object.get("class").and_then(Json::as_str) == Some("Closure")
        && let Some(Json::String(script)) = object.remove("script")
    {
        object.insert("source".into(), json!(sources.script(&script)));
        if let Some(Json::Object(captures)) = object.get_mut("captures") {
            for capture in captures.values_mut() {
                *capture = nested(capture, sources);
            }
        }
    }
    value
}

/// A closure that the compiler writes, which has no place in the script.
pub(crate) fn written(code: String) -> Json {
    json!({"language": "groovy", "code": code})
}

/// The test of a filter or a search: the closure, or Groovy's `isCase` of the value, which
/// matches a pattern, an instance of a class or an equal value.
pub(crate) fn test(criteria: &Criteria, sources: &mut Sources) -> Result<Json, String> {
    Ok(match criteria {
        Criteria::Callback(callback) => closure(callback, sources),
        Criteria::Pattern(pattern) => {
            let pattern = text(pattern);
            written(format!("{{ it -> it.toString() ==~ {pattern} }}"))
        }
        Criteria::Class(class) => written(format!("{{ it -> it instanceof {class} }}")),
        Criteria::Value(value) => {
            let value = literal(value)?;
            written(format!("{{ it -> ({value}).isCase(it) }}"))
        }
    })
}

/// A JSON value as a Groovy literal, for the closures that the compiler writes.
pub(crate) fn literal(value: &Json) -> Result<String, String> {
    Ok(match value {
        Json::Null => "null".into(),
        Json::Bool(value) => value.to_string(),
        Json::Number(number) => number.to_string(),
        Json::String(value) => text(value),
        Json::Array(items) => {
            let items: Result<Vec<String>, String> = items.iter().map(literal).collect();
            format!("[{}]", items?.join(", "))
        }
        Json::Object(object) if object.contains_key("class") => {
            return Err(format!(
                "a {} in code that the compiler writes",
                object["class"]
            ));
        }
        Json::Object(object) if object.is_empty() => "[:]".into(),
        Json::Object(object) => {
            let entries = object
                .iter()
                .map(|(key, value)| literal(value).map(|value| format!("{}: {value}", text(key))));
            let entries: Result<Vec<String>, String> = entries.collect();
            format!("[{}]", entries?.join(", "))
        }
    })
}

/// Text as a single-quoted Groovy string.
pub(crate) fn text(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\n', "\\n");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_groovy_literals() {
        let value = json!([null, true, 1.5, "it's", {"id": "a"}, {}]);
        let written = literal(&value).unwrap();
        assert_eq!(written, r#"[null, true, 1.5, 'it\'s', ['id': 'a'], [:]]"#);
        let file = json!({"class": "File", "location": "/a"});
        assert!(literal(&file).is_err(), "reserved forms have no literal");
    }

    #[test]
    fn names_captures() {
        let callback = Callback {
            script: "/p/main.nf".into(),
            start: 3,
            end: 10,
            code: "{ x -> x + y }".into(),
            parameters: 1,
            captures: vec![("y".into(), json!("!"))],
        };
        let mut sources = Sources::new("/p/main.nf", std::path::Path::new("/p"));
        let closure = closure(&callback, &mut sources);
        assert_eq!(
            closure["scope"],
            json!({"source": "main", "start": 3, "end": 10})
        );
        assert_eq!(closure["captures"], json!({"y": {"value": "!"}}));
    }

    #[test]
    fn names_captured_closures() {
        let inner = json!({"class": "Closure", "script": "/p/lib.nf", "start": 1, "end": 4,
            "code": "{ it }", "captures": {}});
        let mut sources = Sources::new("/p/main.nf", std::path::Path::new("/p"));
        let value = nested(&inner, &mut sources);
        assert_eq!(value["source"], "lib.nf");
        assert_eq!(value.get("script"), None, "the document names no paths");
    }
}
