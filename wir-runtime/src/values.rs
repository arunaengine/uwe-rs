//! The wir value rules: structural equality, the order of numbers and strings, and the
//! canonical form and hash of a value.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cmp::Ordering;

use serde_json::{Map, Value as Json};
use sha2::{Digest, Sha256};

/// Whether two values are equal: records whatever the order of their keys, numbers by value
/// across ints and floats, and Files and Directories by their location.
#[allow(dead_code, reason = "the keyed ops of the next commits use it")]
pub fn equal(left: &Json, right: &Json) -> bool {
    match (left, right) {
        (Json::Number(a), Json::Number(b)) => match (a.as_i64(), b.as_i64()) {
            (Some(a), Some(b)) => a == b,
            _ => a.as_f64() == b.as_f64(),
        },
        (Json::Array(a), Json::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| equal(a, b))
        }
        (Json::Object(a), Json::Object(b)) => match (location(a), location(b)) {
            (Some(a), Some(b)) => a == b,
            (None, None) => {
                a.len() == b.len()
                    && a.iter()
                        .all(|(key, item)| b.get(key).is_some_and(|other| equal(item, other)))
            }
            _ => false,
        },
        _ => left == right,
    }
}

/// The order of two values: numbers by value, strings by their text, and arrays item by item.
/// Other values have no order.
pub fn compare(left: &Json, right: &Json) -> Result<Ordering, String> {
    match (left, right) {
        (Json::Number(a), Json::Number(b)) => match (a.as_i64(), b.as_i64()) {
            (Some(a), Some(b)) => Ok(a.cmp(&b)),
            _ => (a.as_f64().zip(b.as_f64()))
                .and_then(|(a, b)| a.partial_cmp(&b))
                .ok_or_else(|| format!("cannot compare {left} and {right}")),
        },
        (Json::String(a), Json::String(b)) => Ok(a.cmp(b)),
        (Json::Bool(a), Json::Bool(b)) => Ok(a.cmp(b)),
        (Json::Array(a), Json::Array(b)) => {
            for (a, b) in a.iter().zip(b) {
                match compare(a, b)? {
                    Ordering::Equal => continue,
                    other => return Ok(other),
                }
            }
            Ok(a.len().cmp(&b.len()))
        }
        _ => Err(format!("cannot compare {left} and {right}")),
    }
}

/// The text of a value with sorted keys and whole floats as ints, so that equal values have
/// the same text.
pub fn canonical(value: &Json) -> String {
    fn form(value: &Json) -> Json {
        match value {
            Json::Number(number) => match number.as_f64() {
                Some(float) if !number.is_i64() && float.fract() == 0.0 && float.abs() < 9.0e15 => {
                    Json::from(float as i64)
                }
                _ => value.clone(),
            },
            Json::Array(items) => Json::Array(items.iter().map(form).collect()),
            Json::Object(object) => match location(object) {
                Some(location) => Json::String(format!("file:{location}")),
                None => {
                    let mut keys: Vec<&String> = object.keys().collect();
                    keys.sort();
                    let sorted: Map<String, Json> = keys
                        .into_iter()
                        .map(|key| (key.clone(), form(&object[key])))
                        .collect();
                    Json::Object(sorted)
                }
            },
            other => other.clone(),
        }
    }
    form(value).to_string()
}

/// The SHA-256 of the canonical text of a value.
pub fn hash(value: &Json) -> Vec<u8> {
    Sha256::digest(canonical(value).as_bytes()).to_vec()
}

/// The SHA-256 of a value with the bytes of its Files, or `None` when a File cannot be read.
pub fn deep_hash(value: &Json) -> Option<Vec<u8>> {
    let mut hasher = Sha256::new();
    fn walk(value: &Json, hasher: &mut Sha256) -> Option<()> {
        match value {
            Json::Object(object) if object.get("class").and_then(Json::as_str) == Some("File") => {
                let path = location(object)?;
                hasher.update(std::fs::read(path.strip_prefix("file://").unwrap_or(&path)).ok()?);
            }
            Json::Array(items) => items.iter().try_for_each(|item| walk(item, hasher))?,
            other => hasher.update(canonical(other).as_bytes()),
        }
        Some(())
    }
    walk(value, &mut hasher)?;
    Some(hasher.finalize().to_vec())
}

/// The location of a File or Directory value.
fn location(object: &Map<String, Json>) -> Option<String> {
    match object.get("class").and_then(Json::as_str) {
        Some("File" | "Directory") => object
            .get("location")
            .and_then(Json::as_str)
            .map(|text| text.trim_end_matches('/').to_owned()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn wir_rules() {
        assert!(equal(&json!(1), &json!(1.0)), "numbers by value");
        assert!(!equal(&json!(true), &json!(1)), "bools are no numbers");
        assert!(
            equal(&json!({"a": 1, "b": [2]}), &json!({"b": [2.0], "a": 1})),
            "keys in any order"
        );
        let file = |location: &str| json!({"class": "File", "location": location, "basename": "x"});
        assert!(
            equal(&file("/a/x"), &json!({"class": "File", "location": "/a/x"})),
            "a File is its location"
        );
        assert_eq!(compare(&json!(2), &json!(10.5)), Ok(Ordering::Less));
        assert_eq!(compare(&json!("b"), &json!("a")), Ok(Ordering::Greater));
        assert!(
            compare(&json!("a"), &json!(1)).is_err(),
            "no order across kinds"
        );
        assert_eq!(
            hash(&json!({"a": 1, "b": 2.0})),
            hash(&json!({"b": 2, "a": 1}))
        );
        assert_ne!(hash(&json!([1, 2])), hash(&json!([2, 1])));
    }
}
