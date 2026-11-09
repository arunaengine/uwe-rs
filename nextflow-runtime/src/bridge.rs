//! Nextflow values as wir JSON values and back, with the reserved forms of paths, group keys,
//! map entries and sets.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::{Object, PathValue, State, Value};
use serde_json::{Map, Value as Json, json};

/// A value as JSON. A path becomes a File, and group keys, map entries and sets their reserved
/// forms. A value that cannot leave the script, like a closure or a duration, is an error.
pub fn to_json(value: &Value) -> Result<Json, String> {
    Ok(match value {
        Value::Null => Json::Null,
        Value::Boolean(value) => Json::Bool(*value),
        Value::Number(Number::Integer(value)) => json!(value),
        Value::Number(Number::Long(value)) => json!(value),
        Value::Number(number) => match serde_json::Number::from_f64(number.to_f64()) {
            Some(number) => Json::Number(number),
            None => return Err(format!("the number {value} is not finite")),
        },
        Value::String(text) | Value::GString(text) => json!(text.as_ref()),
        Value::List(list) => Json::Array(items(list.borrow().items.iter())?),
        Value::Set(set) => {
            let set = set.borrow();
            json!({"class": "Set", "items": items(set.items.iter().map(|key| &key.0))?})
        }
        Value::MapEntry(entry) => {
            json!({"class": "MapEntry", "key": to_json(&entry.0)?, "value": to_json(&entry.1)?})
        }
        Value::Map(map) => {
            let mut object = Map::new();
            for (key, item) in &map.borrow().entries {
                let (Value::String(name) | Value::GString(name)) = &key.0 else {
                    return Err(format!("the map key {} is no text", key.0));
                };
                object.insert(name.to_string(), to_json(item)?);
            }
            Json::Object(object)
        }
        Value::Path(path) => {
            json!({"class": "File", "location": location(path), "basename": path.name()})
        }
        Value::Object(object) => match &object.borrow().state {
            State::GroupKey(target, size) => {
                json!({"class": "GroupKey", "target": to_json(target)?, "size": size})
            }
            _ => return Err(format!("a {} cannot leave the script", value.class_name())),
        },
        other => return Err(format!("a {} cannot leave the script", other.class_name())),
    })
}

/// The location of a path in wir values: a local path, or the address with its scheme, like
/// `s3://bucket/result.txt`.
pub fn location(path: &PathValue) -> String {
    match path.prefix {
        None => path.path.clone(),
        Some(_) => path.uri(),
    }
}

/// A JSON value as Nextflow sees it: whole numbers as Integer or Long, other numbers as
/// BigDecimal like Groovy literals, Files and Directories as paths.
pub fn from_json(json: &Json) -> Value {
    match json {
        Json::Null => Value::Null,
        Json::Bool(value) => Value::Boolean(*value),
        Json::Number(number) => match number.as_i64() {
            Some(whole) => match i32::try_from(whole) {
                Ok(small) => Value::integer(small),
                Err(_) => Value::Number(Number::Long(whole)),
            },
            None => match Number::parse_decimal(&number.to_string()) {
                Ok(decimal) => Value::Number(decimal),
                Err(_) => Value::Number(Number::Double(number.as_f64().unwrap_or_default())),
            },
        },
        Json::String(text) => Value::string(text.as_str()),
        Json::Array(items) => Value::list(items.iter().map(from_json).collect()),
        Json::Object(object) => reserved(object).unwrap_or_else(|| {
            Value::map(
                (object.iter()).map(|(name, item)| (Value::string(name.as_str()), from_json(item))),
            )
        }),
    }
}

/// The value of a reserved form, `None` for a plain record.
fn reserved(object: &Map<String, Json>) -> Option<Value> {
    let field = |name: &str| object.get(name);
    Some(match field("class")?.as_str()? {
        "File" | "Directory" => {
            let location = field("location")?.as_str()?;
            Value::Path(Rc::new(PathValue::parse(location, "/")))
        }
        "Set" => Value::set(field("items")?.as_array()?.iter().map(from_json)),
        "MapEntry" => {
            let entry = (from_json(field("key")?), from_json(field("value")?));
            Value::MapEntry(Rc::new(entry))
        }
        "GroupKey" => {
            let size = i32::try_from(field("size")?.as_i64()?).ok()?;
            Value::Object(Rc::new(RefCell::new(Object {
                class: "nextflow.extension.GroupKey",
                state: State::GroupKey(from_json(field("target")?), size),
            })))
        }
        _ => return None,
    })
}

fn items<'a>(values: impl Iterator<Item = &'a Value>) -> Result<Vec<Json>, String> {
    values.map(to_json).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_reserved_forms() {
        let text = json!([
            {"id": "a", "single_end": false},
            {"class": "File", "location": "/data/a.fq", "basename": "a.fq"},
            {"class": "GroupKey", "target": "a", "size": 2},
            {"class": "MapEntry", "key": "k", "value": 1},
            {"class": "Set", "items": [1, 2]},
            [1, 3000000000_i64, 0.5, null, "x"]
        ]);
        let value = from_json(&text);
        assert_eq!(
            to_json(&value).unwrap(),
            text,
            "the forms come back unchanged"
        );
        let Value::List(list) = &value else {
            panic!("a list")
        };
        let list = list.borrow();
        assert!(matches!(&list.items[1], Value::Path(path) if path.path == "/data/a.fq"));
        assert!(list.items[2].group_target().is_some(), "a group key");
        let Value::List(numbers) = &list.items[5] else {
            panic!("a list")
        };
        let numbers = &numbers.borrow().items;
        assert!(matches!(numbers[0], Value::Number(Number::Integer(1))));
        assert!(matches!(
            numbers[1],
            Value::Number(Number::Long(3000000000))
        ));
        assert_eq!(
            numbers[2].to_string(),
            "0.5",
            "a decimal as Groovy writes it"
        );
    }

    #[test]
    fn refuses_script_objects() {
        let error = to_json(&Value::Duration(1000)).unwrap_err();
        assert!(error.contains("cannot leave the script"), "{error}");
        let map = Value::map([(Value::integer(1), Value::Null)]);
        assert!(to_json(&map).unwrap_err().contains("no text"));
    }
}
