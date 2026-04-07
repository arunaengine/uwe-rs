//! Data formats: `json`, `yaml` with PyYAML's safe loader rules, `csv` and `gzip`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::collections::HashMap;

use saphyr_parser::{Event, Parser, ScalarStyle};

use super::{function, path_text};
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::number::float_repr;
use crate::value::{Args, Dict, Key, Native, Value};

/// Reads JSON text into values, keeping the order of object keys.
pub fn parse_json(text: &str) -> Result<Value, Error> {
    let mut parser = JsonParser {
        chars: text.chars().collect(),
        index: 0,
    };
    parser.skip_space();
    let value = parser.value()?;
    parser.skip_space();
    if parser.index < parser.chars.len() {
        return Err(parser.error("Extra data"));
    }
    Ok(value)
}

struct JsonParser {
    chars: Vec<char>,
    index: usize,
}

impl JsonParser {
    fn error(&self, message: &str) -> Error {
        let line = self.chars[..self.index.min(self.chars.len())]
            .iter()
            .filter(|c| **c == '\n')
            .count()
            + 1;
        crate::types::library_exception("JSONDecodeError", "json", "ValueError");
        Error::new(
            "JSONDecodeError",
            format!("{message}: line {line} char {}", self.index),
        )
    }

    fn skip_space(&mut self) {
        while self.index < self.chars.len()
            && matches!(self.chars[self.index], ' ' | '\t' | '\n' | '\r')
        {
            self.index += 1;
        }
    }

    fn expect_word(&mut self, word: &str, value: Value) -> Result<Value, Error> {
        let end = self.index + word.chars().count();
        if end <= self.chars.len() && self.chars[self.index..end].iter().collect::<String>() == word
        {
            self.index = end;
            Ok(value)
        } else {
            Err(self.error("Expecting value"))
        }
    }

    fn value(&mut self) -> Result<Value, Error> {
        let Some(&c) = self.chars.get(self.index) else {
            return Err(self.error("Expecting value"));
        };
        match c {
            '{' => {
                self.index += 1;
                let mut dict = Dict::default();
                self.skip_space();
                if self.chars.get(self.index) == Some(&'}') {
                    self.index += 1;
                    return Ok(Value::dict(dict));
                }
                loop {
                    self.skip_space();
                    if self.chars.get(self.index) != Some(&'"') {
                        return Err(self.error("Expecting property name enclosed in double quotes"));
                    }
                    let key = self.string()?;
                    self.skip_space();
                    if self.chars.get(self.index) != Some(&':') {
                        return Err(self.error("Expecting ':' delimiter"));
                    }
                    self.index += 1;
                    self.skip_space();
                    let value = self.value()?;
                    dict.entries.insert(Key(Value::str(key)), value);
                    self.skip_space();
                    match self.chars.get(self.index) {
                        Some(',') => self.index += 1,
                        Some('}') => {
                            self.index += 1;
                            return Ok(Value::dict(dict));
                        }
                        _ => return Err(self.error("Expecting ',' delimiter")),
                    }
                }
            }
            '[' => {
                self.index += 1;
                let mut items = Vec::new();
                self.skip_space();
                if self.chars.get(self.index) == Some(&']') {
                    self.index += 1;
                    return Ok(Value::list(items));
                }
                loop {
                    self.skip_space();
                    items.push(self.value()?);
                    self.skip_space();
                    match self.chars.get(self.index) {
                        Some(',') => self.index += 1,
                        Some(']') => {
                            self.index += 1;
                            return Ok(Value::list(items));
                        }
                        _ => return Err(self.error("Expecting ',' delimiter")),
                    }
                }
            }
            '"' => Ok(Value::str(self.string()?)),
            't' => self.expect_word("true", Value::Bool(true)),
            'f' => self.expect_word("false", Value::Bool(false)),
            'n' => self.expect_word("null", Value::None),
            'N' => self.expect_word("NaN", Value::Float(f64::NAN)),
            'I' => self.expect_word("Infinity", Value::Float(f64::INFINITY)),
            '-' if self.chars.get(self.index + 1) == Some(&'I') => {
                self.index += 1;
                self.expect_word("Infinity", Value::Float(f64::NEG_INFINITY))
            }
            _ => self.number(),
        }
    }

    fn number(&mut self) -> Result<Value, Error> {
        let start = self.index;
        while self.index < self.chars.len()
            && matches!(
                self.chars[self.index],
                '0'..='9' | '-' | '+' | '.' | 'e' | 'E'
            )
        {
            self.index += 1;
        }
        let text: String = self.chars[start..self.index].iter().collect();
        if text.is_empty() {
            return Err(self.error("Expecting value"));
        }
        if text.contains(['.', 'e', 'E']) {
            text.parse::<f64>()
                .map(Value::Float)
                .map_err(|_| self.error("Expecting value"))
        } else {
            crate::number::parse_int(&text, 10).map_err(|_| self.error("Expecting value"))
        }
    }

    fn string(&mut self) -> Result<String, Error> {
        self.index += 1;
        let mut text = String::new();
        loop {
            let Some(&c) = self.chars.get(self.index) else {
                return Err(self.error("Unterminated string starting at"));
            };
            self.index += 1;
            match c {
                '"' => return Ok(text),
                '\\' => {
                    let Some(&escape) = self.chars.get(self.index) else {
                        return Err(self.error("Unterminated string starting at"));
                    };
                    self.index += 1;
                    match escape {
                        'n' => text.push('\n'),
                        't' => text.push('\t'),
                        'r' => text.push('\r'),
                        'b' => text.push('\u{8}'),
                        'f' => text.push('\u{c}'),
                        '/' => text.push('/'),
                        '\\' => text.push('\\'),
                        '"' => text.push('"'),
                        'u' => {
                            let hex: String = self
                                .chars
                                .get(self.index..self.index + 4)
                                .unwrap_or(&[])
                                .iter()
                                .collect();
                            self.index += 4;
                            let mut code = u32::from_str_radix(&hex, 16)
                                .map_err(|_| self.error("Invalid \\uXXXX escape"))?;
                            if (0xD800..0xDC00).contains(&code)
                                && self.chars.get(self.index) == Some(&'\\')
                            {
                                let low: String = self
                                    .chars
                                    .get(self.index + 2..self.index + 6)
                                    .unwrap_or(&[])
                                    .iter()
                                    .collect();
                                if let Ok(low) = u32::from_str_radix(&low, 16) {
                                    code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                                    self.index += 6;
                                }
                            }
                            text.extend(char::from_u32(code));
                        }
                        _ => return Err(self.error("Invalid \\escape")),
                    }
                }
                other => text.push(other),
            }
        }
    }
}

/// `json.dumps` with its main options.
pub fn dump_json(
    interpreter: &mut Interpreter,
    value: &Value,
    indent: Option<&str>,
    sort_keys: bool,
    ensure_ascii: bool,
    depth: usize,
) -> Result<String, Error> {
    let escape = |text: &str| {
        let mut output = String::from("\"");
        for c in text.chars() {
            match c {
                '"' => output.push_str("\\\""),
                '\\' => output.push_str("\\\\"),
                '\n' => output.push_str("\\n"),
                '\r' => output.push_str("\\r"),
                '\t' => output.push_str("\\t"),
                c if (c as u32) < 0x20 => output.push_str(&format!("\\u{:04x}", c as u32)),
                c if ensure_ascii && !c.is_ascii() => {
                    let mut buffer = [0u16; 2];
                    for unit in c.encode_utf16(&mut buffer) {
                        output.push_str(&format!("\\u{unit:04x}"));
                    }
                }
                c => output.push(c),
            }
        }
        output.push('"');
        output
    };
    let (open_space, item_separator, key_separator) = match indent {
        Some(indent) => (
            format!("\n{}", indent.repeat(depth + 1)),
            format!(",\n{}", indent.repeat(depth + 1)),
            ": ",
        ),
        None => (String::new(), ", ".to_owned(), ": "),
    };
    let close_space = match indent {
        Some(indent) => format!("\n{}", indent.repeat(depth)),
        None => String::new(),
    };
    Ok(match value.plain() {
        Value::None => "null".into(),
        Value::Bool(true) => "true".into(),
        Value::Bool(false) => "false".into(),
        Value::Int(number) => number.to_string(),
        Value::BigInt(number) => number.to_string(),
        Value::Float(number) => {
            if number.is_nan() {
                "NaN".into()
            } else if number.is_infinite() {
                if *number > 0.0 {
                    "Infinity"
                } else {
                    "-Infinity"
                }
                .into()
            } else {
                float_repr(*number)
            }
        }
        Value::Str(text) => escape(text),
        Value::List(_) | Value::Tuple(_) => {
            let items = interpreter.iterate(value)?;
            if items.is_empty() {
                return Ok("[]".into());
            }
            let parts = items
                .iter()
                .map(|item| {
                    dump_json(
                        interpreter,
                        item,
                        indent,
                        sort_keys,
                        ensure_ascii,
                        depth + 1,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            format!("[{open_space}{}{close_space}]", parts.join(&item_separator))
        }
        Value::Dict(_) => {
            let mut entries = interpreter.mapping_items(value)?;
            if entries.is_empty() {
                return Ok("{}".into());
            }
            if sort_keys {
                entries.sort_by(|(a, _), (b, _)| {
                    crate::format::plain_str(a).cmp(&crate::format::plain_str(b))
                });
            }
            let mut parts = Vec::new();
            for (key, item) in entries {
                let key = match key.plain() {
                    Value::Str(text) => text.to_string(),
                    Value::Bool(true) => "true".into(),
                    Value::Bool(false) => "false".into(),
                    Value::None => "null".into(),
                    other => crate::format::plain_str(other),
                };
                parts.push(format!(
                    "{}{key_separator}{}",
                    escape(&key),
                    dump_json(
                        interpreter,
                        &item,
                        indent,
                        sort_keys,
                        ensure_ascii,
                        depth + 1
                    )?
                ));
            }
            format!(
                "{{{open_space}{}{close_space}}}",
                parts.join(&item_separator)
            )
        }
        Value::Native(object) if object.instance_of("str") || object.type_name() == "PosixPath" => {
            let text = interpreter.to_str(value)?;
            escape(&text)
        }
        Value::Native(object) if object.iterate(interpreter)?.is_some() => {
            let items = interpreter.iterate(value)?;
            dump_json(
                interpreter,
                &Value::list(items),
                indent,
                sort_keys,
                ensure_ascii,
                depth,
            )?
        }
        other => {
            return Err(Error::type_error(format!(
                "Object of type {} is not JSON serializable",
                crate::types::type_name(other)
            )));
        }
    })
}

fn json_options(
    interpreter: &mut Interpreter,
    args: &mut Args,
) -> Result<(Option<String>, bool, bool), Error> {
    let indent = match args.take_keyword("indent") {
        Some(Value::Int(count)) => Some(" ".repeat(count.max(0) as usize)),
        Some(Value::Str(text)) => Some(text.to_string()),
        _ => None,
    };
    let sort_keys = match args.take_keyword("sort_keys") {
        Some(value) => interpreter.truthy(&value)?,
        None => false,
    };
    let ensure_ascii = match args.take_keyword("ensure_ascii") {
        Some(value) => interpreter.truthy(&value)?,
        None => true,
    };
    args.take_keyword("default");
    args.take_keyword("cls");
    args.take_keyword("separators");
    Ok((indent, sort_keys, ensure_ascii))
}

fn read_all(interpreter: &mut Interpreter, stream: &Value) -> Result<String, Error> {
    if let Some(text) = stream.as_str() {
        return Ok(text.to_owned());
    }
    let read = interpreter.get_attribute(stream, "read")?;
    let text = interpreter.call(&read, Args::default())?;
    match text.plain() {
        Value::Str(text) => Ok(text.to_string()),
        Value::Bytes(bytes) => Ok(String::from_utf8_lossy(bytes).into_owned()),
        other => Ok(crate::format::plain_str(other)),
    }
}
