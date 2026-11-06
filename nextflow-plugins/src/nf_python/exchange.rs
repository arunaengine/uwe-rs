//! The typed JSON of nf-python's `in.json` and `out.json`: `[type, value]` pairs, nested for
//! lists, sets and maps.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::number::{BigDecimal, Number};
use nextflow_evaluator::value::{PathValue, Value};
use serde_json::{Value as Json, json};

/// The argument map of a call as `in.json` holds it.
pub(crate) fn encode_map(arguments: &[(String, Value)]) -> Result<Json, String> {
    let mut pairs = Vec::new();
    for (key, value) in arguments {
        pairs.push(json!([["String", key], encode(value)?]));
    }
    Ok(json!(["Map", pairs]))
}

/// One value as its typed pair; a value of another class fails as the plugin does.
pub(crate) fn encode(value: &Value) -> Result<Json, String> {
    Ok(match value {
        Value::Null => json!(["Null", null]),
        Value::Boolean(value) => json!(["Boolean", value]),
        Value::String(text) | Value::GString(text) => json!(["String", text.as_ref()]),
        Value::Number(Number::Integer(value)) => json!(["Integer", value]),
        Value::Number(Number::Long(value)) => json!(["Integer", value]),
        Value::Number(Number::BigDecimal(_)) => {
            let number: Json = serde_json::from_str(&value.to_string()).unwrap_or(Json::Null);
            json!(["Decimal", number])
        }
        Value::Number(Number::Double(value)) => json!(["Float", float(*value)]),
        Value::Number(Number::Float(value)) => {
            let value: f64 = value.to_string().parse().unwrap_or(f64::from(*value));
            json!(["Float", float(value)])
        }
        Value::List(list) => {
            let items: Result<Vec<Json>, String> = list.borrow().items.iter().map(encode).collect();
            json!(["List", items?])
        }
        Value::Set(set) => {
            let items: Result<Vec<Json>, String> = set
                .borrow()
                .items
                .iter()
                .map(|key| encode(&key.0))
                .collect();
            json!(["Set", items?])
        }
        Value::Map(map) => {
            let mut pairs = Vec::new();
            for (key, value) in &map.borrow().entries {
                pairs.push(json!([encode(&key.0)?, encode(value)?]));
            }
            json!(["Map", pairs])
        }
        Value::Path(_) => json!(["Path", value.to_string()]),
        Value::Duration(millis) => json!(["Duration", millis]),
        Value::MemoryUnit(bytes) => json!(["MemoryUnit", bytes]),
        other => {
            return Err(format!(
                "Cannot serialize type: class {}",
                other.java_class_name()
            ));
        }
    })
}

/// A double as JSON: a number, or the text of an infinity or NaN.
fn float(value: f64) -> Json {
    match value {
        value if value.is_nan() => json!("nan"),
        f64::INFINITY => json!("inf"),
        f64::NEG_INFINITY => json!("-inf"),
        value => json!(value),
    }
}

/// The value of a typed pair of `out.json`; paths are relative to `base`.
pub(crate) fn decode(pair: &Json, base: &str) -> Result<Value, String> {
    let Some([tag, value]) = pair.as_array().map(Vec::as_slice) else {
        return Err(format!(
            "Expected a list of size 2 for unpacking, got: {pair}"
        ));
    };
    let list = || {
        value
            .as_array()
            .ok_or_else(|| format!("Expected a list, got: {value}"))
    };
    let text = || {
        value
            .as_str()
            .ok_or_else(|| format!("Expected a string, got: {value}"))
    };
    Ok(match tag.as_str().unwrap_or_default() {
        "Null" => Value::Null,
        "List" | "Bag" => {
            let items: Result<Vec<Value>, String> =
                list()?.iter().map(|item| decode(item, base)).collect();
            Value::list(items?)
        }
        "Set" => {
            let items: Result<Vec<Value>, String> =
                list()?.iter().map(|item| decode(item, base)).collect();
            Value::set(items?)
        }
        "Map" => {
            let mut entries = Vec::new();
            for entry in list()? {
                let Some([key, value]) = entry.as_array().map(Vec::as_slice) else {
                    return Err(format!(
                        "Expected a list of key-value pairs for Map, got: {entry}"
                    ));
                };
                entries.push((decode(key, base)?, decode(value, base)?));
            }
            Value::map(entries)
        }
        "String" => Value::string(text()?),
        "Integer" => integer(value)?,
        "Decimal" => {
            let digits = match value {
                Json::String(text) => text.clone(),
                Json::Number(number) => number.to_string(),
                _ => return Err(format!("Expected a string, got: {value}")),
            };
            decimal(&digits).ok_or_else(|| format!("Expected a string, got: {value}"))?
        }
        "Float" => match value {
            Json::Number(number) => decimal(&number.to_string())
                .ok_or("Expected a string or number for unpacking float")?,
            Json::String(text) => match text.as_str() {
                "inf" => Value::Number(Number::Double(f64::INFINITY)),
                "-inf" => Value::Number(Number::Double(f64::NEG_INFINITY)),
                "nan" => Value::Number(Number::Double(f64::NAN)),
                _ => {
                    return Err(format!(
                        "Expected a number for special string for Float, got: {text}"
                    ));
                }
            },
            _ => return Err("Expected a string or number for unpacking float".into()),
        },
        "Boolean" => match value {
            Json::Bool(value) => Value::Boolean(*value),
            other => return Err(format!("Expected an boolean, got: {other}")),
        },
        "Path" => Value::Path(Rc::new(PathValue::parse(text()?, base))),
        "Duration" => Value::Duration(value.as_i64().unwrap_or(0)),
        "MemoryUnit" => Value::MemoryUnit(value.as_i64().unwrap_or(0)),
        "VersionNumber" => match list()?.as_slice() {
            [major, minor, patch] => Value::string(format!("{major}.{minor}.{patch}")),
            _ => {
                return Err(format!(
                    "Expected a list of 3 elements for VersionNumber, got: {value}"
                ));
            }
        },
        other => return Err(format!("Unknown type from Python: {other}")),
    })
}

/// An integer of any size; the plugin's limit to 32 bits is not kept.
fn integer(value: &Json) -> Result<Value, String> {
    let digits = match value {
        Json::Number(number) if number.is_i64() || number.is_u64() => number.to_string(),
        _ => return Err(format!("Expected an integer, got: {value}")),
    };
    Number::parse_integer(&digits)
        .map(Value::Number)
        .map_err(|_| format!("Expected an integer, got: {value}"))
}

fn decimal(digits: &str) -> Option<Value> {
    BigDecimal::parse(digits).map(|decimal| Value::Number(Number::BigDecimal(decimal)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_recorded_output() {
        let text = r#"["Map",[[["String","s"],["String","text"]],[["String","i"],["Integer",7]],[["String","bd"],["Decimal","1.25"]],[["String","d"],["Float",2.5]],[["String","b"],["Boolean",true]],[["String","n"],["Null",null]],[["String","list"],["List",[["Integer",1],["String","a"]]]],[["String","set"],["Set",[["Integer",3]]]],[["String","map"],["Map",[[["String","k"],["List",[["Integer",1],["Integer",2]]]]]]],[["String","p"],["Path","/tmp/x.txt"]],[["String","dur"],["Duration",90000]],[["String","mem"],["MemoryUnit",1024]],[["String","inf"],["Float","inf"]]]]"#;
        let value = decode(&serde_json::from_str(text).expect("JSON"), "/").expect("decodes");
        assert_eq!(
            value.to_string(),
            "[s:text, i:7, bd:1.25, d:2.5, b:true, n:null, list:[1, a], set:[3], \
             map:[k:[1, 2]], p:/tmp/x.txt, dur:1m 30s, mem:1 KB, inf:Infinity]"
        );
        let big = decode(&json!(["Integer", 1_099_511_627_776i64]), "/").expect("a Long");
        assert_eq!(big.to_string(), "1099511627776");
        let refused = decode(&json!(["Tuple", []]), "/").expect_err("unknown");
        assert_eq!(refused, "Unknown type from Python: Tuple");
        let refused = decode(&json!(["Map", [1]]), "/").expect_err("not a pair");
        assert_eq!(
            refused,
            "Expected a list of key-value pairs for Map, got: 1"
        );
    }

    #[test]
    fn encodes_recorded_input() {
        let arguments = vec![
            ("s".to_owned(), Value::string("text")),
            ("i".to_owned(), Value::integer(7)),
            ("d".to_owned(), Value::Number(Number::Double(2.5))),
            ("f".to_owned(), Value::Number(Number::Float(1.5))),
            ("n".to_owned(), Value::Null),
            ("set".to_owned(), Value::set([Value::integer(3)])),
            ("dur".to_owned(), Value::Duration(90_000)),
            ("mem".to_owned(), Value::MemoryUnit(1024)),
            (
                "inf".to_owned(),
                Value::Number(Number::Double(f64::INFINITY)),
            ),
        ];
        let encoded = encode_map(&arguments).expect("encodes").to_string();
        assert_eq!(
            encoded,
            r#"["Map",[[["String","s"],["String","text"]],[["String","i"],["Integer",7]],[["String","d"],["Float",2.5]],[["String","f"],["Float",1.5]],[["String","n"],["Null",null]],[["String","set"],["Set",[["Integer",3]]]],[["String","dur"],["Duration",90000]],[["String","mem"],["MemoryUnit",1024]],[["String","inf"],["Float","inf"]]]]"#
        );
        let refused = encode(&Value::Date(0)).expect_err("a date");
        assert_eq!(refused, "Cannot serialize type: class java.util.Date");
    }
}
