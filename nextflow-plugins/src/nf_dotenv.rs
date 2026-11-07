//! nf-dotenv 2.0.1: `dotenv(key)` reads a value from a dotenv file next to the main script.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::{PathValue, Value};

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["dotenv"],
    call: Some(call),
    scopes: &["dotenv"],
    ..Plugin::new("nf-dotenv", Kind::Functions, State::Implemented)
};

/// `dotenv(key)`: the variable of the engine's environment, else the last value of the key in
/// the file. The file is read on each call, so a missing or bad file fails every call.
fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    let [key] = arguments.as_slice() else {
        return Err(Error::Unsupported {
            message: format!(
                "the function `{name}` of the plugin `nf-dotenv` with these arguments"
            ),
            span: Default::default(),
        });
    };
    let key = key.to_string();
    let path = file_path(context);
    let entries = read(&path, context).map_err(|inner| {
        let message = format!(
            "Could not find dotenv file at path {path}\n\nConsider modifying the following \
             properties in your Nextflow config:\n\n\tdotenv.filename = '.env'\n\
             \tdotenv.relative = '.'\n\nOriginal `dotenv` parsing error: {inner}"
        );
        thrown(Exception::new("IllegalStateException", message))
    })?;
    if let Some(value) = context.host.environment(&key) {
        return Ok(Value::string(value));
    }
    match entries.iter().rev().find(|(name, _)| *name == key) {
        Some((_, value)) => Ok(Value::string(value.as_str())),
        None => {
            let message = format!("Could not find key {key} in dotenv {path}");
            Err(thrown(Exception::illegal_argument(message)))
        }
    }
}

/// The file of the config's `dotenv.relative` and `dotenv.filename`, below the directory of
/// the main script; empty settings count as unset.
fn file_path(context: &Context) -> String {
    let setting = |name: &str, default: &str| {
        let value = context.config.get(&["dotenv", name]).map(|v| v.to_string());
        value
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| default.into())
    };
    let relative = setting("relative", ".");
    let directory = match relative.starts_with('/') {
        true => relative,
        false => format!("{}/{relative}", context.project_dir.trim_end_matches('/')),
    };
    format!("{directory}/{}", setting("filename", ".env"))
}

/// The entries of the file, or the message of why it cannot be read.
fn read(path: &str, context: &mut Context) -> Result<Vec<(String, String)>, String> {
    let file = PathValue::parse(path, "/");
    let missing = format!("Could not find {path} on the classpath");
    match context.host.metadata(&file) {
        Ok(Some(metadata)) if !metadata.directory => {}
        _ => return Err(missing),
    }
    let bytes = context.host.read(&file).map_err(|_| missing)?;
    parse(&String::from_utf8_lossy(&bytes))
}

/// The entries of a dotenv file by the rules of dotenv-java 3.2.0: no `export`, no escapes,
/// no expansion; double quotes may span lines, single quotes stay in the value.
fn parse(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut entries = Vec::new();
    let mut collected: Option<String> = None;
    for line in lines(text) {
        if collected.is_none()
            && (line.trim().is_empty() || line.starts_with('#') || line.starts_with("////"))
        {
            continue;
        }
        let text = match collected.take() {
            Some(earlier) => format!("{earlier}\n{line}"),
            None => line.to_owned(),
        };
        let Some((key, value)) = entry(&text) else {
            return Err(format!("Malformed entry {text}"));
        };
        if value.starts_with('"') && !value.ends_with('"') {
            collected = Some(text);
            continue;
        }
        let value = value.trim_matches(|c: char| c <= ' ');
        if (value.starts_with('"') || value.ends_with('"')) && !quoted(value) {
            return Err(format!("Malformed entry, unmatched quotes {line}"));
        }
        let value = match value.len() > 1 && value.starts_with('"') && value.ends_with('"') {
            true => &value[1..value.len() - 1],
            false => value,
        };
        entries.push((key.to_owned(), value.to_owned()));
    }
    Ok(entries)
}

/// The lines of a text, ended by LF, CR or CRLF, as Java's `BufferedReader` reads them.
fn lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut rest = text;
    while let Some(end) = rest.find(['\n', '\r']) {
        lines.push(&rest[..end]);
        let skip = if rest[end..].starts_with("\r\n") {
            2
        } else {
            1
        };
        rest = &rest[end + skip..];
    }
    if !rest.is_empty() {
        lines.push(rest);
    }
    lines
}

/// The key and the value as written of an entry `key = value # comment`, `None` when the text
/// has no such shape. A value in quotes may hold `#`; without quotes `#` starts the comment.
fn entry(text: &str) -> Option<(&str, &str)> {
    let space = |c: char| matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r');
    let rest = text.trim_start_matches(space);
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')))
        .unwrap_or(rest.len());
    let (key, rest) = rest.split_at(end);
    let rest = rest.trim_start_matches(space).strip_prefix('=')?;
    let rest = rest.trim_start_matches(space);
    if key.is_empty() {
        return None;
    }
    // Only a comment without a line break may follow the value.
    let tail = |after: &str| {
        let after = after.trim_start_matches(space);
        after.is_empty() || after.starts_with('#') && !after.contains(['\n', '\r'])
    };
    for quote in ['\'', '"'] {
        if let Some(inside) = rest.strip_prefix(quote)
            && let Some(close) = inside.find(quote)
            && tail(&inside[close + 1..])
        {
            return Some((key, &rest[..close + 2]));
        }
    }
    let end = rest.find('#').unwrap_or(rest.len());
    tail(&rest[end..]).then_some((key, &rest[..end]))
}

/// Whether a value is in double quotes with no unescaped double quote inside.
fn quoted(value: &str) -> bool {
    if value.len() < 2 || !value.starts_with('"') || !value.ends_with('"') {
        return false;
    }
    let inside = &value[1..value.len() - 1];
    let mut previous = None;
    for c in inside.chars() {
        if c == '"' && previous != Some('\\') {
            return false;
        }
        previous = Some(c);
    }
    true
}

fn thrown(exception: Exception) -> Error {
    Error::Thrown {
        exception: Rc::new(exception),
        span: Default::default(),
    }
}
