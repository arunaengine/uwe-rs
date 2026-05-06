//! The attributes that Snakemake's file objects have beyond those of `str`. Snakemake gives
//! workflow code the files of rules as `_IOFile`, a subclass of `str`, and the folder of the
//! current Snakefile as a `SourceFile`. Here they are texts tagged with `rule` or
//! `source_file`, which behave like texts and get these attributes on top.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use python_evaluator::{Args, AttributeHook, Error, Interpreter, Value};

use crate::pattern;

pub struct FileAttributes;

impl AttributeHook for FileAttributes {
    fn attribute(
        &self,
        interpreter: &mut Interpreter,
        value: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let Some(text) = value.as_str().map(str::to_owned) else {
            return Ok(None);
        };
        if value.tag("source_file").is_some() {
            return Ok(source_file_attribute(&text, name));
        }
        if value.tag("rule").is_none() {
            return Ok(None);
        }
        let flagged = |flag: &str| Value::Bool(value.tag(flag).is_some());
        Ok(Some(match name {
            "file" => Value::str(text),
            "is_directory" => flagged("directory"),
            "is_temp" => flagged("temp"),
            "is_ancient" => flagged("ancient"),
            "is_multiext" => flagged("multiext"),
            "is_storage" => Value::Bool(false),
            "open" => Value::builtin("open", move |interpreter, mut args| {
                let mode = match args.take(0, "mode") {
                    Some(mode) => interpreter.to_str(&mode)?,
                    None => "r".into(),
                };
                let path = interpreter.host.resolve(&text);
                if interpreter.host.stat(&path).is_none() {
                    return Err(crate::workflow::workflow_error(format!(
                        "Error: Input file {text} could not be opened, as it does not exist."
                    )));
                }
                python_evaluator::builtins::open_file(interpreter, &text, &mode, false)
            }),
            "exists" => {
                let path = interpreter.host.resolve(&text);
                let exists = interpreter.host.stat(&path).is_some();
                Value::builtin("exists", move |_, _| Ok(Value::Bool(exists)))
            }
            "contains_wildcard" => {
                let found = !pattern::wildcard_names(&text).is_empty();
                Value::builtin("contains_wildcard", move |_, _| Ok(Value::Bool(found)))
            }
            "get_wildcard_names" => {
                let names = pattern::wildcard_names(&text);
                Value::builtin("get_wildcard_names", move |_, _| {
                    Ok(Value::set(
                        names
                            .iter()
                            .map(|name| python_evaluator::Key(Value::str(name.as_str())))
                            .collect(),
                    ))
                })
            }
            _ => return Ok(None),
        }))
    }
}

/// An attribute of the `SourceFile` of a local folder or file.
fn source_file_attribute(text: &str, name: &str) -> Option<Value> {
    let text = text.to_owned();
    Some(match name {
        "path" => Value::str(text),
        "get_path_or_uri" | "get_filename" | "abspath" | "simplify_path" => {
            let result = Value::str(text).with_tag("source_file", Value::Bool(true));
            Value::builtin(name, move |_, _| Ok(result.clone()))
        }
        "join" => Value::builtin("join", move |interpreter, args: Args| {
            let part = args.get(0).cloned().unwrap_or(Value::None);
            let part = python_evaluator::modules::path_text(interpreter, &part)?;
            let joined = std::path::Path::new(&text).join(part);
            Ok(Value::str(joined.to_string_lossy().as_ref())
                .with_tag("source_file", Value::Bool(true)))
        }),
        "is_local" => Value::builtin("is_local", |_, _| Ok(Value::Bool(true))),
        _ => return None,
    })
}
