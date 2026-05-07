//! Data formats: `json`, `yaml` with PyYAML's safe loader rules, `csv` and `gzip`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use saphyr_parser::{Event, Parser, ScalarStyle};

use super::{function, path_text};
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::number::float_repr;
use crate::value::{Args, Dict, Key, Value};

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

pub fn json() -> Vec<(&'static str, Value)> {
    let decode_error = crate::types::library_exception("JSONDecodeError", "json", "ValueError");
    vec![
        ("JSONDecodeError", Value::Class(decode_error)),
        (
            "loads",
            function("loads", |interpreter, args| {
                args.expect("loads", 1, 1)?;
                let text = match args.positional[0].plain() {
                    Value::Str(text) => text.to_string(),
                    Value::Bytes(bytes) => String::from_utf8_lossy(bytes).into_owned(),
                    other => read_all(interpreter, other)?,
                };
                parse_json(&text)
            }),
        ),
        (
            "load",
            function("load", |interpreter, args| {
                args.expect("load", 1, 1)?;
                let text = read_all(interpreter, &args.positional[0])?;
                parse_json(&text)
            }),
        ),
        (
            "dumps",
            function("dumps", |interpreter, mut args| {
                let (indent, sort_keys, ensure_ascii) = json_options(interpreter, &mut args)?;
                args.expect("dumps", 1, 1)?;
                Ok(Value::str(dump_json(
                    interpreter,
                    &args.positional[0],
                    indent.as_deref(),
                    sort_keys,
                    ensure_ascii,
                    0,
                )?))
            }),
        ),
        (
            "dump",
            function("dump", |interpreter, mut args| {
                let (indent, sort_keys, ensure_ascii) = json_options(interpreter, &mut args)?;
                args.expect("dump", 2, 2)?;
                let text = dump_json(
                    interpreter,
                    &args.positional[0],
                    indent.as_deref(),
                    sort_keys,
                    ensure_ascii,
                    0,
                )?;
                let write = interpreter.get_attribute(&args.positional[1], "write")?;
                interpreter.call(&write, Args::new(vec![Value::str(text)]))?;
                Ok(Value::None)
            }),
        ),
    ]
}

/// Reads YAML text as PyYAML's `safe_load` does: YAML 1.1 scalars, anchors and merge keys.
pub fn load_yaml(text: &str) -> Result<Value, Error> {
    let documents = load_yaml_all(text, true)?;
    Ok(documents.into_iter().next().unwrap_or(Value::None))
}

struct Collection {
    mapping: bool,
    anchor: usize,
    items: Vec<Value>,
    merges: Vec<usize>,
}

/// Reads the documents of a YAML stream; with `first`, only the first one.
pub fn load_yaml_all(text: &str, first: bool) -> Result<Vec<Value>, Error> {
    let mut documents = Vec::new();
    let mut stack: Vec<Collection> = Vec::new();
    let mut anchors: HashMap<usize, Value> = HashMap::new();
    let mut root: Option<Value> = None;
    let mut started = false;
    for event in Parser::new_from_str(text) {
        let (event, marker) = event.map_err(|error| {
            crate::types::library_exception("YAMLError", "yaml", "Exception");
            Error::new(
                "YAMLError",
                format!(
                    "{} at line {} column {}",
                    error.info(),
                    error.marker().line(),
                    error.marker().col() + 1
                ),
            )
        })?;
        let _ = marker;
        let (value, anchor) = match event {
            Event::DocumentStart(_) => {
                started = true;
                continue;
            }
            Event::DocumentEnd => {
                if started {
                    documents.push(root.take().unwrap_or(Value::None));
                    started = false;
                    if first {
                        break;
                    }
                }
                continue;
            }
            Event::SequenceStart(anchor, _) => {
                stack.push(Collection {
                    mapping: false,
                    anchor,
                    items: Vec::new(),
                    merges: Vec::new(),
                });
                continue;
            }
            Event::MappingStart(anchor, _) => {
                stack.push(Collection {
                    mapping: true,
                    anchor,
                    items: Vec::new(),
                    merges: Vec::new(),
                });
                continue;
            }
            Event::SequenceEnd | Event::MappingEnd => {
                let collection = stack.pop().expect("an open collection");
                let anchor = collection.anchor;
                (collection_value(collection)?, anchor)
            }
            Event::Scalar(text, style, anchor, tag) => {
                let tagged = tag
                    .as_ref()
                    .map(|tag| format!("{}{}", tag.handle, tag.suffix));
                let value = match (style, tagged.as_deref()) {
                    (_, Some("!!str" | "tag:yaml.org,2002:str")) => Value::str(text.as_ref()),
                    (ScalarStyle::Plain, _) => {
                        if let Some(top) = stack.last_mut()
                            && top.mapping
                            && top.items.len() % 2 == 0
                            && text == "<<"
                        {
                            top.merges.push(top.items.len());
                        }
                        yaml_scalar(&text)
                    }
                    _ => Value::str(text.as_ref()),
                };
                (value, anchor)
            }
            Event::Alias(anchor) => match anchors.get(&anchor) {
                Some(value) => (value.clone(), 0),
                None => return Err(Error::new("YAMLError", "found undefined alias")),
            },
            _ => continue,
        };
        if anchor > 0 {
            anchors.insert(anchor, value.clone());
        }
        match stack.last_mut() {
            Some(collection) => collection.items.push(value),
            None => root = Some(value),
        }
    }
    if started {
        documents.push(root.take().unwrap_or(Value::None));
    }
    Ok(documents)
}

fn collection_value(collection: Collection) -> Result<Value, Error> {
    if !collection.mapping {
        return Ok(Value::list(collection.items));
    }
    let mut dict = Dict::default();
    let mut merged: Vec<(Key, Value)> = Vec::new();
    for (index, pair) in collection.items.chunks(2).enumerate() {
        let value = pair.get(1).cloned().unwrap_or(Value::None);
        if collection.merges.contains(&(index * 2)) {
            let sources = match value.plain() {
                Value::List(list) => list.borrow().clone(),
                other => vec![other.clone()],
            };
            for source in sources {
                if let Value::Dict(source) = source {
                    for (key, value) in source.borrow().entries.iter() {
                        merged.push((key.clone(), value.clone()));
                    }
                }
            }
            continue;
        }
        let key = Key::new(pair[0].clone())
            .map_err(|_| Error::new("YAMLError", "found unhashable key"))?;
        dict.entries.insert(key, value);
    }
    for (key, value) in merged {
        dict.entries.entry(key).or_insert(value);
    }
    Ok(Value::dict(dict))
}

fn all_digits(text: &str, radix: u32) -> bool {
    !text.is_empty() && text.chars().all(|c| c == '_' || c.is_digit(radix))
}

/// A plain scalar as PyYAML's resolver reads it.
pub fn yaml_scalar(text: &str) -> Value {
    match text {
        "" | "~" | "null" | "Null" | "NULL" => return Value::None,
        "yes" | "Yes" | "YES" | "true" | "True" | "TRUE" | "on" | "On" | "ON" => {
            return Value::Bool(true);
        }
        "no" | "No" | "NO" | "false" | "False" | "FALSE" | "off" | "Off" | "OFF" => {
            return Value::Bool(false);
        }
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" => {
            return Value::Float(f64::INFINITY);
        }
        "-.inf" | "-.Inf" | "-.INF" => return Value::Float(f64::NEG_INFINITY),
        ".nan" | ".NaN" | ".NAN" => return Value::Float(f64::NAN),
        _ => {}
    }
    let (sign, body) = match text.as_bytes()[0] {
        b'-' => (-1i64, &text[1..]),
        b'+' => (1, &text[1..]),
        _ => (1, text),
    };
    let negate = |value: Value| {
        if sign < 0 {
            match value {
                Value::Int(number) => Value::Int(-number),
                Value::BigInt(number) => Value::from_bigint(-(*number).clone()),
                Value::Float(number) => Value::Float(-number),
                other => other,
            }
        } else {
            value
        }
    };
    let clean = |digits: &str| digits.replace('_', "");
    if let Some(binary) = body.strip_prefix("0b")
        && all_digits(binary, 2)
        && let Ok(value) = crate::number::parse_int(&clean(binary), 2)
    {
        return negate(value);
    }
    if let Some(hex) = body.strip_prefix("0x")
        && all_digits(hex, 16)
        && let Ok(value) = crate::number::parse_int(&clean(hex), 16)
    {
        return negate(value);
    }
    if body.len() > 1
        && body.starts_with('0')
        && all_digits(&body[1..], 8)
        && let Ok(value) = crate::number::parse_int(&clean(&body[1..]), 8)
    {
        return negate(value);
    }
    if (body == "0"
        || (body.starts_with(|c: char| c.is_ascii_digit() && c != '0') && all_digits(body, 10)))
        && let Ok(value) = crate::number::parse_int(&clean(body), 10)
    {
        return negate(value);
    }
    // Sexagesimal integers like 1:30.
    if body.contains(':') && !body.contains('.') {
        let parts: Vec<&str> = body.split(':').collect();
        if parts[0].starts_with(|c: char| c.is_ascii_digit() && c != '0')
            && parts.iter().all(|part| all_digits(part, 10))
            && parts[1..]
                .iter()
                .all(|part| part.len() <= 2 && part.parse::<u32>().is_ok_and(|n| n < 60))
        {
            let mut total: i64 = 0;
            for part in parts {
                total = total * 60 + clean(part).parse::<i64>().unwrap_or(0);
            }
            return Value::Int(sign * total);
        }
    }
    // Floats need a point; an exponent needs a sign.
    if is_yaml_float(body)
        && let Ok(value) = clean(body).parse::<f64>()
    {
        return Value::Float(sign as f64 * value);
    }
    Value::str(text)
}

fn is_yaml_float(body: &str) -> bool {
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(index) => (&body[..index], Some(&body[index + 1..])),
        None => (body, None),
    };
    if let Some(exponent) = exponent {
        let digits = match exponent.strip_prefix(['+', '-']) {
            Some(digits) => digits,
            None => return false,
        };
        if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
    }
    let Some((whole, fraction)) = mantissa.split_once('.') else {
        return false;
    };
    let whole_ok = whole.is_empty()
        || (whole.starts_with(|c: char| c.is_ascii_digit()) && all_digits(whole, 10));
    let fraction_ok = fraction.chars().all(|c| c.is_ascii_digit() || c == '_');
    whole_ok && fraction_ok && !(whole.is_empty() && fraction.is_empty())
}

/// `yaml.dump` in block style, as PyYAML writes simple data.
pub fn dump_yaml(
    interpreter: &mut Interpreter,
    value: &Value,
    sort_keys: bool,
) -> Result<String, Error> {
    let mut output = String::new();
    write_yaml(interpreter, value, 0, sort_keys, &mut output, false)?;
    if !output.ends_with('\n') {
        output.push('\n');
    }
    Ok(output)
}

fn yaml_text(interpreter: &mut Interpreter, value: &Value) -> Result<String, Error> {
    Ok(match value.plain() {
        Value::None => "null".into(),
        Value::Bool(true) => "true".into(),
        Value::Bool(false) => "false".into(),
        Value::Int(_) | Value::BigInt(_) => crate::format::plain_repr(value),
        Value::Float(number) => {
            if number.is_nan() {
                ".nan".into()
            } else if number.is_infinite() {
                if *number > 0.0 { ".inf" } else { "-.inf" }.into()
            } else {
                float_repr(*number)
            }
        }
        Value::Str(text) => {
            let plain = yaml_scalar(text);
            let needs_quotes = !matches!(plain, Value::Str(_))
                || text.is_empty()
                || text
                    .starts_with(|c: char| "!&*-?:,[]{}#|>@`\"'%".contains(c) || c.is_whitespace())
                || text.ends_with(char::is_whitespace)
                || text.contains(": ")
                || text.contains(" #")
                || text.contains('\n');
            if needs_quotes {
                format!("'{}'", text.replace('\'', "''"))
            } else {
                text.to_string()
            }
        }
        other => interpreter.to_str(other)?,
    })
}

fn write_yaml(
    interpreter: &mut Interpreter,
    value: &Value,
    indent: usize,
    sort_keys: bool,
    output: &mut String,
    inline_start: bool,
) -> Result<(), Error> {
    let pad = " ".repeat(indent);
    match value.plain() {
        Value::Dict(_) => {
            let mut entries = interpreter.mapping_items(value)?;
            if entries.is_empty() {
                output.push_str("{}\n");
                return Ok(());
            }
            if sort_keys {
                entries.sort_by(|(a, _), (b, _)| {
                    crate::format::plain_str(a).cmp(&crate::format::plain_str(b))
                });
            }
            for (index, (key, item)) in entries.into_iter().enumerate() {
                if index > 0 || !inline_start {
                    output.push_str(&pad);
                }
                output.push_str(&yaml_text(interpreter, &key)?);
                output.push(':');
                write_nested(interpreter, &item, indent, sort_keys, output)?;
            }
        }
        Value::List(_) | Value::Tuple(_) => {
            let items = interpreter.iterate(value)?;
            if items.is_empty() {
                output.push_str("[]\n");
                return Ok(());
            }
            for (index, item) in items.into_iter().enumerate() {
                if index > 0 || !inline_start {
                    output.push_str(&pad);
                }
                output.push_str("- ");
                match item.plain() {
                    Value::Dict(_) | Value::List(_) | Value::Tuple(_)
                        if interpreter.truthy(&item)? =>
                    {
                        write_yaml(interpreter, &item, indent + 2, sort_keys, output, true)?;
                    }
                    _ => {
                        output.push_str(&yaml_text(interpreter, &item)?);
                        output.push('\n');
                    }
                }
            }
        }
        other => {
            output.push_str(&yaml_text(interpreter, other)?);
            output.push('\n');
        }
    }
    Ok(())
}

fn write_nested(
    interpreter: &mut Interpreter,
    item: &Value,
    indent: usize,
    sort_keys: bool,
    output: &mut String,
) -> Result<(), Error> {
    match item.plain() {
        Value::Dict(_) if interpreter.truthy(item)? => {
            output.push('\n');
            write_yaml(interpreter, item, indent + 2, sort_keys, output, false)
        }
        Value::List(_) | Value::Tuple(_) if interpreter.truthy(item)? => {
            output.push('\n');
            write_yaml(interpreter, item, indent, sort_keys, output, false)
        }
        _ => {
            output.push(' ');
            write_yaml(interpreter, item, indent, sort_keys, output, true)
        }
    }
}

pub fn yaml() -> Vec<(&'static str, Value)> {
    let error = crate::types::library_exception("YAMLError", "yaml", "Exception");
    let loader = |name: &str| Value::str(format!("yaml.{name}"));
    fn load(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
        args.take_keyword("Loader");
        let stream = args
            .positional
            .first()
            .cloned()
            .ok_or_else(|| Error::type_error("load() missing stream"))?;
        let text = read_all(interpreter, &stream)?;
        load_yaml(&text)
    }
    fn load_all(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
        args.take_keyword("Loader");
        let stream = args
            .positional
            .first()
            .cloned()
            .ok_or_else(|| Error::type_error("load_all() missing stream"))?;
        let text = read_all(interpreter, &stream)?;
        Ok(Value::iterator(load_yaml_all(&text, false)?))
    }
    fn dump(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
        let sort_keys = match args.take_keyword("sort_keys") {
            Some(value) => interpreter.truthy(&value)?,
            None => true,
        };
        for option in [
            "default_flow_style",
            "indent",
            "width",
            "Dumper",
            "allow_unicode",
            "explicit_start",
            "encoding",
        ] {
            args.take_keyword(option);
        }
        let stream = args
            .take_keyword("stream")
            .or_else(|| args.positional.get(1).cloned());
        let text = dump_yaml(interpreter, &args.positional[0], sort_keys)?;
        match stream {
            Some(stream) if !stream.is_none() => {
                let write = interpreter.get_attribute(&stream, "write")?;
                interpreter.call(&write, Args::new(vec![Value::str(text)]))?;
                Ok(Value::None)
            }
            _ => Ok(Value::str(text)),
        }
    }
    vec![
        ("YAMLError", Value::Class(error)),
        ("SafeLoader", loader("SafeLoader")),
        ("FullLoader", loader("FullLoader")),
        ("Loader", loader("Loader")),
        ("BaseLoader", loader("BaseLoader")),
        ("UnsafeLoader", loader("UnsafeLoader")),
        ("CSafeLoader", loader("CSafeLoader")),
        ("CLoader", loader("CLoader")),
        ("SafeDumper", loader("SafeDumper")),
        ("Dumper", loader("Dumper")),
        ("load", function("load", load)),
        ("safe_load", function("safe_load", load)),
        ("full_load", function("full_load", load)),
        ("unsafe_load", function("unsafe_load", load)),
        ("load_all", function("load_all", load_all)),
        ("safe_load_all", function("safe_load_all", load_all)),
        ("dump", function("dump", dump)),
        ("safe_dump", function("safe_dump", dump)),
    ]
}

pub fn gzip() -> Vec<(&'static str, Value)> {
    vec![(
        "open",
        function("open", |interpreter, mut args| {
            let mode = args
                .take(1, "mode")
                .and_then(|mode| mode.as_str().map(str::to_owned))
                .unwrap_or_else(|| "rb".into());
            args.take_keyword("encoding");
            let path = args
                .positional
                .first()
                .cloned()
                .ok_or_else(|| Error::type_error("open() missing file"))?;
            let path = path_text(interpreter, &path)?;
            if mode.contains(['w', 'a', 'x']) {
                return Err(Error::unsupported(
                    "writing gzip files while the workflow is planned",
                ));
            }
            crate::builtins::open_file(interpreter, &path, &mode, true)
        }),
    )]
}
