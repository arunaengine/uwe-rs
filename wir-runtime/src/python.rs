//! Python callbacks: callables and expressions of Snakemake sources, run by the Python
//! evaluator of uwe, and the values that cross between wir and Python.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::rc::Rc;

use indexmap::IndexSet;
use python_evaluator::{Args, Dict, Interpreter, Key, LocalHost, Value};
use serde_json::{Map, Value as Json, json};

/// Python code with the library that runs before it.
pub struct Code<'c> {
    pub code: &'c str,
    pub library: &'c [String],
}

impl Code<'_> {
    /// The value of the code as an expression, with `values` as its globals.
    pub fn value(&self, values: &Map<String, Json>, base: &Path) -> Result<Json, String> {
        let (mut interpreter, globals) = self.prepared(values, base)?;
        let result = interpreter
            .eval_text(self.code, &globals)
            .map_err(|error| error.to_string())?;
        to_json(&mut interpreter, &result)
    }

    /// Calls the callable that the code gives with `arguments`.
    #[allow(dead_code, reason = "the stream ops of the next commits call it")]
    pub fn call(
        &self,
        values: &Map<String, Json>,
        arguments: &[Json],
        base: &Path,
    ) -> Result<Json, String> {
        let (mut interpreter, globals) = self.prepared(values, base)?;
        let callable = interpreter
            .eval_text(self.code, &globals)
            .map_err(|error| error.to_string())?;
        let arguments = Args::new(arguments.iter().map(to_value).collect());
        let result = interpreter
            .call(&callable, arguments)
            .map_err(|error| error.to_string())?;
        to_json(&mut interpreter, &result)
    }

    /// An interpreter whose globals hold the values and the definitions of the library.
    fn prepared(
        &self,
        values: &Map<String, Json>,
        base: &Path,
    ) -> Result<(Interpreter, Rc<python_evaluator::Scope>), String> {
        let mut interpreter = Interpreter::new(Rc::new(LocalHost::new(base)));
        let globals = interpreter.new_globals("wir", None);
        for (name, value) in values {
            globals.set(name, to_value(value));
        }
        for (index, text) in self.library.iter().enumerate() {
            let path = format!("<library {index}>");
            interpreter
                .exec_text(&path, text, &globals)
                .map_err(|error| error.to_string())?;
        }
        Ok((interpreter, globals))
    }
}

/// A wir value as Python sees it: Files and Directories are their paths, a set is a set.
pub fn to_value(json: &Json) -> Value {
    match json {
        Json::Null => Value::None,
        Json::Bool(flag) => Value::Bool(*flag),
        Json::Number(number) => match number.as_i64() {
            Some(int) => Value::Int(int),
            None => Value::Float(number.as_f64().unwrap_or_default()),
        },
        Json::String(text) => Value::str(text.as_str()),
        Json::Array(items) => Value::list(items.iter().map(to_value).collect()),
        Json::Object(object) => match object.get("class").and_then(Json::as_str) {
            Some("File" | "Directory") => {
                let location = object
                    .get("location")
                    .and_then(Json::as_str)
                    .unwrap_or_default();
                Value::str(location.strip_prefix("file://").unwrap_or(location))
            }
            Some("Set") => {
                let items = object
                    .get("items")
                    .and_then(Json::as_array)
                    .cloned()
                    .unwrap_or_default();
                Value::set(
                    items
                        .iter()
                        .map(|item| Key(to_value(item)))
                        .collect::<IndexSet<Key>>(),
                )
            }
            _ => {
                let mut dict = Dict::default();
                for (key, item) in object {
                    dict.entries
                        .insert(Key(Value::str(key.as_str())), to_value(item));
                }
                Value::dict(dict)
            }
        },
    }
}

/// A Python value as a wir value, or why it cannot cross a port.
pub fn to_json(interpreter: &mut Interpreter, value: &Value) -> Result<Json, String> {
    let failed = |error: python_evaluator::Error| error.to_string();
    Ok(match value.plain() {
        Value::None => Json::Null,
        Value::Bool(flag) => Json::Bool(*flag),
        Value::Int(int) => json!(int),
        Value::BigInt(big) => return Err(format!("the integer {big} is out of the int range")),
        Value::Float(float) => match serde_json::Number::from_f64(*float) {
            Some(number) => Json::Number(number),
            None => return Err(format!("the number {float} has no wir value")),
        },
        Value::Str(text) => json!(text.as_ref()),
        Value::List(_) | Value::Tuple(_) => {
            let items = interpreter.iterate(value).map_err(failed)?;
            Json::Array(
                items
                    .iter()
                    .map(|item| to_json(interpreter, item))
                    .collect::<Result<_, _>>()?,
            )
        }
        Value::Set(_) | Value::FrozenSet(_) => {
            let items = interpreter.iterate(value).map_err(failed)?;
            let items: Vec<Json> = items
                .iter()
                .map(|item| to_json(interpreter, item))
                .collect::<Result<_, _>>()?;
            json!({"class": "Set", "items": items})
        }
        Value::Dict(_) => {
            let mut object = Map::new();
            for (key, item) in interpreter.mapping_items(value).map_err(failed)? {
                let Value::Str(key) = key.plain() else {
                    return Err("a dict with keys other than strings cannot cross a port".into());
                };
                object.insert(key.to_string(), to_json(interpreter, &item)?);
            }
            Json::Object(object)
        }
        other => {
            let shown = interpreter.repr(other).map_err(failed)?;
            return Err(format!("the Python value {shown} cannot cross a port"));
        }
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn python_callbacks() {
        let base = Path::new("/data");
        let scaled = Code {
            code: "lambda x: x * factor",
            library: &[],
        };
        let values = Map::from_iter([("factor".into(), json!(3))]);
        assert_eq!(scaled.call(&values, &[json!(7)], base).unwrap(), json!(21));
        let library = vec!["def label(sample, n):\n    return f'{sample}-{n}'\n".to_owned()];
        let labelled = Code {
            code: "label",
            library: &library,
        };
        assert_eq!(
            labelled
                .call(&Map::new(), &[json!("a"), json!(2)], base)
                .unwrap(),
            json!("a-2")
        );
        let expression = Code {
            code: "sorted(inputs['names'])[0].upper()",
            library: &[],
        };
        let values = Map::from_iter([("inputs".into(), json!({"names": ["b", "a"]}))]);
        assert_eq!(expression.value(&values, base).unwrap(), json!("A"));
        let file = json!({"class": "File", "location": "/data/a.txt"});
        let path = Code {
            code: "lambda f: f.endswith('.txt')",
            library: &[],
        };
        assert_eq!(
            path.call(&Map::new(), &[file], base).unwrap(),
            json!(true),
            "a File is its path"
        );
        let set = Code {
            code: "lambda items: set(items)",
            library: &[],
        };
        assert_eq!(
            set.call(&Map::new(), &[json!([1, 1, 2])], base).unwrap(),
            json!({"class": "Set", "items": [1, 2]})
        );
        let function = Code {
            code: "lambda: (lambda y: y)",
            library: &[],
        };
        let error = function.call(&Map::new(), &[], base).unwrap_err();
        assert!(error.contains("cannot cross a port"), "{error}");
        assert!(
            Code {
                code: "lambda x:",
                library: &[]
            }
            .call(&Map::new(), &[], base)
            .is_err()
        );
    }
}
