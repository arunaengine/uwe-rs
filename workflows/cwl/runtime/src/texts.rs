//! Reads the texts of a job as files and directories where the type of the input asks for one.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_parser::model::Type;

use crate::files;
use crate::types::Types;
use crate::value::{Map, Value};

/// Turns a text in `value` into a `File` or `Directory` object where `value_type` asks for one,
/// also inside lists, unions and records. The text resolves against `base` like the `path` of
/// a file object. A union keeps a text that one of its other members takes.
pub fn read(types: &Types, value_type: &Type, value: &mut Value, base: &str) {
    match (types.resolve(value_type), &mut *value) {
        (Type::File, Value::String(text)) => *value = object("File", text, base),
        (Type::Directory, Value::String(text)) => *value = object("Directory", text, base),
        (Type::Array(array), Value::List(items)) => {
            for item in items {
                read(types, &array.items, item, base);
            }
        }
        (Type::Record(record), Value::Map(map)) => {
            for field in &record.fields {
                if let Some(item) = map.get_mut(&field.name) {
                    read(types, &field.value_type, item, base);
                }
            }
        }
        (Type::Union(members), value) if !types.matches(value_type, value) => {
            for member in members {
                let mut attempt = value.clone();
                read(types, member, &mut attempt, base);
                if types.matches(member, &attempt) {
                    *value = attempt;
                    return;
                }
            }
        }
        _ => {}
    }
}

fn object(class: &str, text: &str, base: &str) -> Value {
    let mut map = Map::new();
    map.insert("class".into(), Value::string(class));
    map.insert(
        "location".into(),
        Value::String(files::resolve_location(base, text)),
    );
    Value::Map(map)
}
