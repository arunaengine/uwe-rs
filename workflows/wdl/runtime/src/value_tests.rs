//! Checks coercion of WDL values and their JSON form.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::json;
use wdl_analysis::types::{ArrayType, CompoundType, MapType, PairType, PrimitiveType, Type};

use super::Value;

fn primitive(kind: PrimitiveType, optional: bool) -> Type {
    Type::Primitive(kind, optional)
}

fn array(element: Type, non_empty: bool) -> Type {
    let array = match non_empty {
        true => ArrayType::non_empty(element),
        false => ArrayType::new(element),
    };
    Type::Compound(CompoundType::Array(array), false)
}

#[test]
fn coerces_like_wdl() {
    let float = primitive(PrimitiveType::Float, false);
    assert_eq!(Value::Int(2).coerce(&float), Ok(Value::Float(2.0)));
    let file = primitive(PrimitiveType::File, false);
    let path = Value::String("a.txt".into()).coerce(&file);
    assert_eq!(path, Ok(Value::File("a.txt".into())));
    let optional = primitive(PrimitiveType::Integer, true);
    assert_eq!(Value::None.coerce(&optional), Ok(Value::None));
    let required = primitive(PrimitiveType::Integer, false);
    assert!(Value::None.coerce(&required).is_err());
    assert!(Value::String("1".into()).coerce(&required).is_err());
    let non_empty = array(required.clone(), true);
    assert!(Value::Array(Vec::new()).coerce(&non_empty).is_err());
}

#[test]
fn reads_typed_json() {
    let base = Path::new("/data");
    let files = array(primitive(PrimitiveType::File, false), false);
    let value = Value::from_json(&json!(["a.txt", "s3://b/c"]), &files, base);
    let expected = vec![
        Value::File("/data/a.txt".into()),
        Value::File("s3://b/c".into()),
    ];
    assert_eq!(value, Ok(Value::Array(expected)));
    let string = primitive(PrimitiveType::String, false);
    let pair = Type::Compound(
        CompoundType::Pair(PairType::new(string.clone(), files)),
        false,
    );
    let value = Value::from_json(&json!({"left": "x", "right": []}), &pair, base);
    let expected = Value::Pair(
        Box::new(Value::String("x".into())),
        Box::new(Value::Array(vec![])),
    );
    assert_eq!(value, Ok(expected));
    let integer = primitive(PrimitiveType::Integer, false);
    let map = Type::Compound(CompoundType::Map(MapType::new(string, integer)), false);
    let value = Value::from_json(&json!({"a": 1}), &map, base).expect("a map");
    assert_eq!(value.to_json(), json!({"a": 1}));
}

#[test]
fn formats_floats() {
    assert_eq!(Value::Float(1.5).text().as_deref(), Some("1.500000"));
    assert_eq!(Value::Array(vec![]).text(), None);
}

#[test]
fn shapes_result_values() {
    let pair = Value::Pair(
        Box::new(Value::File("/out/a/x.txt".into())),
        Box::new(Value::Directory("/out/d/".into())),
    );
    let expected = json!([
        {"class": "File", "location": "/out/a/x.txt", "basename": "x.txt"},
        {"class": "Directory", "location": "/out/d/", "basename": "d"}
    ]);
    assert_eq!(pair.result_json(), expected, "a pair as an array of two");
    assert_eq!(Value::Int(3).result_json(), json!(3));
}
