//! Methods of paths that do not read the file system, from Nextflow's `FilesEx` and the JDK.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use crate::value::{PathValue, Value};

pub(super) fn path_method(path: &Rc<PathValue>, name: &str, arguments: &[Value]) -> Option<Value> {
    let text = |value: &Value| match value {
        Value::String(text) | Value::GString(text) => Some(text.to_string()),
        Value::Path(other) => Some(other.uri()),
        _ => None,
    };
    let path_value = |path: PathValue| Some(Value::Path(Rc::new(path)));
    match (name, arguments) {
        ("getName", []) => Some(Value::string(path.name())),
        ("getBaseName", []) => Some(Value::string(path.base_name())),
        ("getBaseName", [Value::Number(times)]) => {
            let mut name = path.name().to_owned();
            for _ in 0..times.to_i64() {
                match name.rfind('.') {
                    Some(dot) => name.truncate(dot),
                    None => break,
                }
            }
            Some(Value::string(name))
        }
        ("getSimpleName", []) => Some(Value::string(path.simple_name())),
        ("getExtension", []) => Some(Value::string(path.extension())),
        ("getParent", []) => Some(
            path.parent()
                .map_or(Value::Null, |p| Value::Path(Rc::new(p))),
        ),
        ("getScheme", []) => Some(Value::string(path.scheme())),
        ("toUriString", []) => Some(Value::string(path.uri())),
        ("toAbsolutePath" | "normalize" | "toRealPath", []) => Some(Value::Path(path.clone())),
        ("resolve", [other]) => path_value(path.resolve(&text(other)?)),
        ("resolveSibling", [other]) => path_value(path.resolve_sibling(&text(other)?)),
        _ => None,
    }
}
