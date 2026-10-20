//! Groovy's JSON classes: `JsonOutput.toJson` and `prettyPrint`, `JsonGenerator` with its
//! options, and `JsonSlurper`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use nextflow_parser::Span;

use super::super::access::instance_of;
use super::super::{AtSpan, Error, Interpreter, thrown, unsupported};
use crate::exception::Exception;
use crate::number::{BigDecimal, Number};
use crate::value::{JsonOptions, Object, State, Value, simple_class_name};

impl Interpreter<'_> {
    /// The JSON text of a value, as Groovy's `DefaultJsonGenerator` writes it.
    pub(in crate::interpreter) fn json_text(
        &mut self,
        value: &Value,
        options: &JsonOptions,
        span: Span,
    ) -> Result<String, Error> {
        let mut out = String::new();
        self.write_json(value, options, &mut out, span)?;
        Ok(out)
    }

    fn write_json(
        &mut self,
        value: &Value,
        options: &JsonOptions,
        out: &mut String,
        span: Span,
    ) -> Result<(), Error> {
        for (class, closure) in &options.converters {
            if instance_of(value, simple_class_name(class)) {
                let converted = self.call_closure_value(closure, vec![value.clone()], span)?;
                let rest = JsonOptions {
                    exclude_nulls: options.exclude_nulls,
                    converters: Vec::new(),
                };
                return self.write_json(&converted, &rest, out, span);
            }
        }
        match value {
            Value::Null => out.push_str("null"),
            Value::Boolean(value) => out.push_str(if *value { "true" } else { "false" }),
            Value::Number(Number::Double(value)) if !value.is_finite() => {
                let message = format!("Number {} can't be serialized as JSON", value);
                return Err(thrown(Exception::illegal_argument(message), span));
            }
            Value::Number(number) => out.push_str(&number.to_string()),
            Value::String(text) | Value::GString(text) => escaped(text, out),
            Value::List(_) | Value::Set(_) => {
                out.push('[');
                for (index, item) in super::elements(value).iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    self.write_json(item, options, out, span)?;
                }
                out.push(']');
            }
            Value::Map(map) => {
                let entries: Vec<(Value, Value)> = map
                    .borrow()
                    .entries
                    .iter()
                    .map(|(key, value)| (key.0.clone(), value.clone()))
                    .collect();
                out.push('{');
                let mut first = true;
                for (key, value) in entries {
                    if options.exclude_nulls && matches!(value, Value::Null) {
                        continue;
                    }
                    if !first {
                        out.push(',');
                    }
                    first = false;
                    escaped(&key.to_string(), out);
                    out.push(':');
                    self.write_json(&value, options, out, span)?;
                }
                out.push('}');
            }
            // Groovy writes the properties of these objects, as of Java beans.
            Value::MemoryUnit(bytes) => out.push_str(&format!(
                "{{\"bytes\":{bytes},\"giga\":{},\"kilo\":{},\"mega\":{}}}",
                bytes >> 30,
                bytes >> 10,
                bytes >> 20
            )),
            Value::Duration(millis) => out.push_str(&format!(
                "{{\"durationInMillis\":{millis},\"days\":{},\"hours\":{},\"millis\":{millis},\
                 \"minutes\":{},\"seconds\":{},\
                 \"units\":[\"DAYS\",\"HOURS\",\"MINUTES\",\"SECONDS\",\"MILLIS\"]}}",
                millis / 86_400_000,
                millis / 3_600_000,
                millis / 60_000,
                millis / 1000
            )),
            other => {
                let message = format!("JSON output of a {}", other.class_name());
                return Err(unsupported(message, span));
            }
        }
        Ok(())
    }

    /// Methods of the stood-in objects: the JSON classes, templates and `workflow`.
    pub(in crate::interpreter) fn object_method(
        &mut self,
        object: &Rc<RefCell<Object>>,
        name: &str,
        arguments: &[Value],
        span: Span,
    ) -> Result<Option<Value>, Error> {
        // A group key stands for its key.
        let group = match &object.borrow().state {
            State::GroupKey(target, size) => Some((target.clone(), *size)),
            _ => None,
        };
        if let Some((target, size)) = group {
            return Ok(Some(match (name, arguments) {
                ("getGroupSize", []) => Value::integer(size),
                ("getGroupTarget", []) => target,
                _ => self.call_method(&target, name, arguments.to_vec(), span)?,
            }));
        }
        if matches!(object.borrow().state, State::Stream(..)) {
            return self.stream_method(object, name, arguments, span);
        }
        if let Some(value) = self.org_json_method(object, name, arguments, span)? {
            return Ok(Some(value));
        }
        let builder = match &object.borrow().state {
            State::Builder(text) => Some(text.clone()),
            _ => None,
        };
        if let Some(text) = builder {
            // Methods that only read the text are those of a string.
            return match super::builder::builder_method(object, name, arguments, span)? {
                Some(value) => Ok(Some(value)),
                None => self.string_method(&text, name, arguments, span),
            };
        }
        if let State::Writer(text) = &mut object.borrow_mut().state {
            return Ok(Some(match (name, arguments) {
                ("write" | "print" | "append", [value]) | ("leftShift", [value]) => {
                    text.push_str(&value.to_string());
                    if name == "leftShift" || name == "append" {
                        Value::Object(object.clone())
                    } else {
                        Value::Null
                    }
                }
                ("println", [value]) => {
                    text.push_str(&value.to_string());
                    text.push('\n');
                    Value::Null
                }
                ("newLine" | "println", []) => {
                    text.push('\n');
                    Value::Null
                }
                ("flush" | "close", []) => Value::Null,
                _ => return Ok(None),
            }));
        }
        match &object.borrow().state {
            State::Time(time) => {
                let result = super::time::time_method(*time, name, arguments);
                return result.transpose().at(span);
            }
            State::Bytes(bytes) => return Ok(super::bytes::bytes_method(bytes, name, arguments)),
            State::Digest(algorithm) => {
                return Ok(super::bytes::digest_method(algorithm, name, arguments));
            }
            _ => {}
        }
        let class = object.borrow().class;
        let text = match &object.borrow().state {
            State::Text(text) => Some(text.clone()),
            _ => None,
        };
        match (class, name, arguments, text) {
            ("java.time.format.DateTimeFormatter", "withZone", [_], _) => {
                return Ok(Some(Value::Object(object.clone())));
            }
            // A formatter formats a `java.time` value as the value formats itself.
            ("java.time.format.DateTimeFormatter", "format", [time @ Value::Object(_)], _) => {
                let formatter = Value::Object(object.clone());
                return self.library_method(time, "format", &[formatter], span);
            }
            ("java.text.SimpleDateFormat", "format", [Value::Date(millis)], Some(text)) => {
                return Ok(Some(Value::string(
                    crate::date::format(*millis, &text).at(span)?,
                )));
            }
            ("java.time.format.DateTimeFormatter", "format", [Value::Date(millis)], Some(text)) => {
                return Ok(Some(Value::string(
                    crate::date::format(*millis, &text).at(span)?,
                )));
            }
            ("java.text.NumberFormat", "parse", [source], _) => {
                return Ok(Some(
                    super::number::parse_grouped(&source.to_string()).at(span)?,
                ));
            }
            _ => {}
        }
        let builder = object.borrow().class == "groovy.json.JsonGenerator$Options";
        let slurper = object.borrow().class == "groovy.yaml.YamlSlurper";
        let mut target = object.borrow_mut();
        let value = match (&mut target.state, name, arguments) {
            (State::Json(options), "excludeNulls", []) if builder => {
                options.exclude_nulls = true;
                Value::Object(object.clone())
            }
            (
                State::Json(options),
                "addConverter",
                [Value::Class(class), Value::Closure(closure)],
            ) if builder => {
                options.converters.push((class.clone(), closure.clone()));
                Value::Object(object.clone())
            }
            (State::Json(options), "build", []) if builder => {
                Value::Object(Rc::new(RefCell::new(Object {
                    class: "groovy.json.DefaultJsonGenerator",
                    state: State::Json(options.clone()),
                })))
            }
            (State::Json(options), "toJson", [value]) if !builder => {
                let options = options.clone();
                drop(target);
                Value::string(self.json_text(value, &options, span)?)
            }
            (State::Text(text), "toPrettyString", []) if !builder => {
                Value::string(pretty_print(text).at(span)?)
            }
            (State::JsonSlurper, "parseText", [text]) => {
                let text = text.to_string();
                if text.is_empty() {
                    let message = "Text must not be null or empty";
                    return Err(thrown(Exception::illegal_argument(message), span));
                }
                parse(&text).at(span)?
            }
            (State::JsonSlurper, "parse", [Value::Path(path)]) => {
                let bytes = self.host.read(path).map_err(|error| error.or_at(span))?;
                parse(&String::from_utf8_lossy(&bytes)).at(span)?
            }
            (State::Workflow(workflow), "onComplete", [Value::Closure(handler)]) => {
                workflow.on_complete.push(handler.clone());
                Value::Null
            }
            (State::Uri(text), part, []) if part.starts_with("get") && part != "getClass" => {
                let part = part.trim_start_matches("get").to_ascii_lowercase();
                if part == "port" {
                    let port = super::uri_part(text, "port").and_then(|p| p.parse::<i32>().ok());
                    Value::integer(port.unwrap_or(-1))
                } else {
                    super::uri_part(text, &part).map_or(Value::Null, Value::string)
                }
            }
            (State::Uri(text), "toString", []) => Value::string(text.as_ref()),
            (State::Uri(text), "resolve", [reference]) => {
                super::uri(&super::resolve_uri(text, &reference.to_string()))
            }
            // Groovy's `YamlSlurper` reads YAML as JSON, where decimals are `BigDecimal`.
            (State::Yaml, "parse" | "parseText", [source]) if slurper => {
                let text = match source {
                    Value::Path(path) => {
                        let bytes = self.host.read(path).map_err(|error| error.or_at(span))?;
                        String::from_utf8_lossy(&bytes).into_owned()
                    }
                    other => other.to_string(),
                };
                decimals(super::yaml::load(&text).at(span)?)
            }
            (State::Yaml, "load", [source]) => {
                let text = match source {
                    Value::Path(path) => {
                        let bytes = self.host.read(path).map_err(|error| error.or_at(span))?;
                        String::from_utf8_lossy(&bytes).into_owned()
                    }
                    // A reader, like `new FileReader(path)`, from its position on.
                    Value::Object(stream) if matches!(stream.borrow().state, State::Stream(..)) => {
                        let State::Stream(bytes, position) = &stream.borrow().state else {
                            unreachable!("a stream")
                        };
                        String::from_utf8_lossy(&bytes[(*position).min(bytes.len())..]).into_owned()
                    }
                    other => other.to_string(),
                };
                super::yaml::load(&text).at(span)?
            }
            (State::Yaml, "dumpAsMap", [map]) => Value::string(super::yaml::dump_as_map(map)),
            (State::ConfigSlurper, "parse", [text]) => {
                let text = match text {
                    Value::Path(path) => {
                        let bytes = self.host.read(path).map_err(|error| error.or_at(span))?;
                        String::from_utf8_lossy(&bytes).into_owned()
                    }
                    other => other.to_string(),
                };
                drop(target);
                self.slurp(&text, span)?
            }
            (State::TemplateEngine, "createTemplate", [text]) => {
                let text = match text {
                    Value::Path(path) => {
                        let bytes = self.host.read(path).map_err(|error| error.or_at(span))?;
                        String::from_utf8_lossy(&bytes).into_owned()
                    }
                    other => other.to_string(),
                };
                Value::Object(Rc::new(RefCell::new(Object {
                    class: "groovy.text.SimpleTemplateEngine$SimpleTemplate",
                    state: State::Template(text.into()),
                })))
            }
            (State::Template(text), "make", binding) => {
                let text = text.clone();
                let binding = match binding {
                    [Value::Map(map)] => map
                        .borrow()
                        .entries
                        .iter()
                        .map(|(key, value)| {
                            (
                                Rc::from(key.0.to_string()),
                                Rc::new(RefCell::new(value.clone())),
                            )
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                drop(target);
                Value::string(self.render_template(&text, binding, span)?)
            }
            (State::Workflow(workflow), "toMap", []) => {
                crate::interpreter::library::copy_map(&workflow.properties)
            }
            (State::Workflow(workflow), "onError", [Value::Closure(handler)]) => {
                workflow.on_error.push(handler.clone());
                Value::Null
            }
            _ => return Ok(None),
        };
        Ok(Some(value))
    }
}

impl Interpreter<'_> {
    /// Methods of input streams and readers: lines and text from the position on.
    fn stream_method(
        &mut self,
        object: &Rc<RefCell<Object>>,
        name: &str,
        arguments: &[Value],
        span: Span,
    ) -> Result<Option<Value>, Error> {
        let (bytes, position) = match &object.borrow().state {
            State::Stream(bytes, position) => (bytes.clone(), *position),
            _ => unreachable!("a stream"),
        };
        let rest = String::from_utf8_lossy(&bytes[position.min(bytes.len())..]).into_owned();
        let finish = |object: &Rc<RefCell<Object>>, position: usize| {
            object.borrow_mut().state = State::Stream(bytes.clone(), position);
        };
        Ok(Some(match (name, arguments) {
            // The next byte, or -1 at the end.
            ("read", []) => match bytes.get(position) {
                Some(byte) => {
                    finish(object, position + 1);
                    Value::integer(i32::from(*byte))
                }
                None => Value::integer(-1),
            },
            ("readLine", []) => {
                if rest.is_empty() {
                    return Ok(Some(Value::Null));
                }
                let line = rest.split_inclusive('\n').next().unwrap_or_default();
                finish(object, position + line.len());
                Value::string(line.trim_end_matches(['\n', '\r']))
            }
            ("eachLine", [Value::Closure(f)]) => {
                finish(object, bytes.len());
                let mut last = Value::Null;
                for (index, line) in rest.lines().enumerate() {
                    let mut arguments = vec![Value::string(line)];
                    if f.code.parameters.len() == 2 {
                        arguments.push(Value::integer(index as i32 + 1));
                    }
                    last = self.call_closure_value(f, arguments, span)?;
                }
                last
            }
            ("getText", []) => {
                finish(object, bytes.len());
                Value::string(rest)
            }
            ("readLines", []) => {
                finish(object, bytes.len());
                Value::list(rest.lines().map(Value::string).collect())
            }
            ("withReader" | "withStream", [Value::Closure(f)]) => {
                self.call_closure_value(f, vec![Value::Object(object.clone())], span)?
            }
            ("close", []) => Value::Null,
            _ => return Ok(None),
        }))
    }
}

/// A JSON string in quotes, with Groovy's escapes: control characters and everything outside
/// printable ASCII become `\uXXXX`.
pub(super) fn escaped(text: &str, out: &mut String) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(character),
            other => {
                let mut units = [0u16; 2];
                for unit in other.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
}

/// `JsonOutput.prettyPrint`: the same JSON, indented by four spaces.
pub(super) fn pretty_print(text: &str) -> Result<String, Exception> {
    let tokens = tokens(text)?;
    let mut out = String::new();
    let mut indent = 0usize;
    for token in tokens {
        match token {
            Token::Open(bracket) => {
                indent += 4;
                out.push(bracket);
                out.push('\n');
                out.push_str(&" ".repeat(indent));
            }
            Token::Close(bracket) => {
                indent = indent.saturating_sub(4);
                out.push('\n');
                out.push_str(&" ".repeat(indent));
                out.push(bracket);
            }
            Token::Comma => {
                out.push_str(",\n");
                out.push_str(&" ".repeat(indent));
            }
            Token::Colon => out.push_str(": "),
            Token::String(text) => escaped(&text, &mut out),
            Token::Other(text) => out.push_str(&text),
        }
    }
    Ok(out)
}

/// `JsonSlurper.parseText`: objects become maps and arrays lists. Whole numbers become an
/// `Integer`, else a `Long` (which wraps, as in Groovy); other numbers a `BigDecimal`.
pub fn parse(text: &str) -> Result<Value, Exception> {
    if text.trim().is_empty() {
        let message = "Unable to determine the current character, it is not a string, number, \
                       array, or object";
        return Err(Exception::new("JsonException", message));
    }
    parse_with(text, slurper_number)
}

/// JSON as Nextflow's `splitJson` reads it: a number of digits with an optional fraction
/// becomes an `Integer`, a `Long` or a `Double`, whichever holds it first; any other number,
/// like `-1` or `1e5`, stays text.
pub fn parse_for_split(text: &str) -> Result<Value, Exception> {
    parse_with(text, splitter_number)
}

fn parse_with(
    text: &str,
    number: fn(&str) -> Result<Value, Exception>,
) -> Result<Value, Exception> {
    let tokens = tokens(text)?;
    let mut position = 0;
    let value = parse_value(&tokens, &mut position, number)?;
    if position != tokens.len() {
        return Err(json_error("unexpected text after the value"));
    }
    Ok(value)
}

fn slurper_number(number: &str) -> Result<Value, Exception> {
    Ok(if number.contains(['.', 'e', 'E']) {
        let decimal = BigDecimal::parse(number)
            .ok_or_else(|| json_error(&format!("invalid number {number}")))?;
        Value::Number(Number::BigDecimal(decimal))
    } else {
        match number.parse::<i32>() {
            Ok(value) => Value::Number(Number::Integer(value)),
            Err(_) => {
                let wide: i128 = number
                    .parse()
                    .map_err(|_| json_error(&format!("invalid number {number}")))?;
                Value::Number(Number::Long(wide as i64))
            }
        }
    })
}

fn splitter_number(number: &str) -> Result<Value, Exception> {
    let (whole, fraction) = number.split_once('.').unwrap_or((number, "0"));
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    if !(digits(whole) && digits(fraction)) {
        return Ok(Value::string(number));
    }
    Ok(if let Ok(value) = number.parse::<i32>() {
        Value::Number(Number::Integer(value))
    } else if let Ok(value) = number.parse::<i64>() {
        Value::Number(Number::Long(value))
    } else {
        match number.parse::<f64>() {
            Ok(value) => Value::Number(Number::Double(value)),
            Err(_) => Value::string(number),
        }
    })
}

fn parse_value(
    tokens: &[Token],
    position: &mut usize,
    number: fn(&str) -> Result<Value, Exception>,
) -> Result<Value, Exception> {
    let token = tokens
        .get(*position)
        .ok_or_else(|| json_error("unexpected end"))?;
    *position += 1;
    Ok(match token {
        Token::Open('{') => {
            let mut entries = Vec::new();
            if tokens.get(*position) == Some(&Token::Close('}')) {
                *position += 1;
                return Ok(Value::map(entries));
            }
            loop {
                let Some(Token::String(key)) = tokens.get(*position) else {
                    return Err(json_error("expected a key"));
                };
                *position += 1;
                if tokens.get(*position) != Some(&Token::Colon) {
                    return Err(json_error("expected `:`"));
                }
                *position += 1;
                let value = parse_value(tokens, position, number)?;
                entries.push((Value::string(key.as_str()), value));
                match tokens.get(*position) {
                    Some(Token::Comma) => *position += 1,
                    Some(Token::Close('}')) => {
                        *position += 1;
                        break;
                    }
                    _ => return Err(json_error("expected `,` or `}`")),
                }
            }
            Value::map(entries)
        }
        Token::Open(_) => {
            let mut items = Vec::new();
            if tokens.get(*position) == Some(&Token::Close(']')) {
                *position += 1;
                return Ok(Value::list(items));
            }
            loop {
                items.push(parse_value(tokens, position, number)?);
                match tokens.get(*position) {
                    Some(Token::Comma) => *position += 1,
                    Some(Token::Close(']')) => {
                        *position += 1;
                        break;
                    }
                    _ => return Err(json_error("expected `,` or `]`")),
                }
            }
            Value::list(items)
        }
        Token::String(text) => Value::string(text.as_str()),
        Token::Other(text) => match text.as_str() {
            "true" => Value::Boolean(true),
            "false" => Value::Boolean(false),
            "null" => Value::Null,
            other => number(other)?,
        },
        _ => return Err(json_error("unexpected token")),
    })
}

#[derive(Debug, PartialEq)]
enum Token {
    Open(char),
    Close(char),
    Comma,
    Colon,
    /// A string, with its escapes resolved.
    String(String),
    /// A number, `true`, `false` or `null`.
    Other(String),
}

fn tokens(text: &str) -> Result<Vec<Token>, Exception> {
    let mut tokens = Vec::new();
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '{' | '[' => tokens.push(Token::Open(character)),
            '}' | ']' => tokens.push(Token::Close(character)),
            ',' => tokens.push(Token::Comma),
            ':' => tokens.push(Token::Colon),
            '"' => {
                let mut value = String::new();
                loop {
                    match characters.next() {
                        None => return Err(json_error("unterminated string")),
                        Some('"') => break,
                        Some('\\') => match characters.next() {
                            Some('n') => value.push('\n'),
                            Some('r') => value.push('\r'),
                            Some('t') => value.push('\t'),
                            Some('b') => value.push('\u{8}'),
                            Some('f') => value.push('\u{c}'),
                            Some('u') => {
                                let hex: String = characters.by_ref().take(4).collect();
                                let unit = u16::from_str_radix(&hex, 16)
                                    .map_err(|_| json_error("invalid escape"))?;
                                let units = [unit];
                                value.extend(
                                    char::decode_utf16(units).map(|c| c.unwrap_or('\u{fffd}')),
                                );
                            }
                            Some(other) => value.push(other),
                            None => return Err(json_error("unterminated string")),
                        },
                        Some(other) => value.push(other),
                    }
                }
                tokens.push(Token::String(value));
            }
            c if c.is_whitespace() => {}
            _ => {
                let mut value = String::from(character);
                while let Some(next) =
                    characters.next_if(|c| c.is_ascii_alphanumeric() || "+-.".contains(*c))
                {
                    value.push(next);
                }
                tokens.push(Token::Other(value));
            }
        }
    }
    Ok(tokens)
}

fn json_error(message: &str) -> Exception {
    Exception::new("JsonException", format!("Invalid JSON: {message}"))
}

/// A value with its doubles as `BigDecimal`, as JSON reads them.
fn decimals(value: Value) -> Value {
    match value {
        Value::Number(number @ crate::number::Number::Double(_)) => {
            Value::Number(crate::number::Number::BigDecimal(number.to_big_decimal()))
        }
        Value::List(list) => {
            let items = list.borrow().items.clone();
            Value::list(items.into_iter().map(decimals).collect())
        }
        Value::Map(map) => {
            let entries = map.borrow().entries.clone();
            Value::map(
                entries
                    .into_iter()
                    .map(|(key, value)| (key.0, decimals(value))),
            )
        }
        other => other,
    }
}
