//! Stand-ins for `fileinput`, `filecmp` and `tarfile.is_tarfile`, which read files that exist
//! while a workflow is planned.
// Copyright (c) 2026 The uwe Contributors
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

/// `tarfile.is_tarfile(name)`: whether a file is a tar archive, plain or compressed with
/// gzip, by the `ustar` mark of its first header, as the archives of today have it.
fn is_tarfile(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let name = args.get(0).cloned().unwrap_or(Value::None);
    let path = path_text(interpreter, &name)?;
    let resolved = interpreter.host.resolve(&path);
    let bytes = interpreter
        .host
        .read(&resolved)
        .map_err(|error| Error::os_error(&error, &path))?;
    let header = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut header = Vec::new();
        let decoder = flate2::read::MultiGzDecoder::new(bytes.as_slice());
        std::io::Read::read_to_end(&mut std::io::Read::take(decoder, 512), &mut header)
            .unwrap_or_default();
        header
    } else if bytes.starts_with(b"BZh") || bytes.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0]) {
        return Err(Error::unsupported(
            "tarfile.is_tarfile of a file compressed with bzip2 or xz",
        ));
    } else {
        bytes
    };
    Ok(Value::Bool(header.get(257..262) == Some(b"ustar")))
}

pub fn tarfile() -> Vec<(&'static str, Value)> {
    vec![("is_tarfile", function("is_tarfile", is_tarfile))]
}
