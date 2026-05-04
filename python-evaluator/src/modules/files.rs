//! Stand-ins for `fileinput` and `filecmp`, which read files that exist while a workflow is
//! planned.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::{function, path_text};
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Args, Value};

/// The text of a file that the host can read.
fn read_text(interpreter: &mut Interpreter, path: &Value) -> Result<String, Error> {
    let path = path_text(interpreter, path)?;
    let resolved = interpreter.host.resolve(&path);
    let bytes = interpreter
        .host
        .read(&resolved)
        .map_err(|error| Error::os_error(&error, &path))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// `fileinput.input(files)`: the lines of the files, one after another.
fn input(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let files = args.take(0, "files").unwrap_or(Value::None);
    let files = match files.plain() {
        Value::None => Vec::new(),
        Value::Str(_) => vec![files.clone()],
        _ => interpreter.iterate(&files)?,
    };
    let mut lines = Vec::new();
    for file in &files {
        let text = read_text(interpreter, file)?;
        lines.extend(text.split_inclusive('\n').map(Value::str));
    }
    Ok(Value::iterator(lines))
}

pub fn fileinput() -> Vec<(&'static str, Value)> {
    vec![
        ("input", function("input", input)),
        ("close", function("close", |_, _| Ok(Value::None))),
    ]
}

pub fn filecmp() -> Vec<(&'static str, Value)> {
    vec![(
        "cmp",
        function("cmp", |interpreter, args| {
            let first = args.get(0).cloned().unwrap_or(Value::None);
            let second = args.get(1).cloned().unwrap_or(Value::None);
            let same = read_text(interpreter, &first)? == read_text(interpreter, &second)?;
            Ok(Value::Bool(same))
        }),
    )]
}
