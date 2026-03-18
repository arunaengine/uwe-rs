//! Methods of `String`: Java's, Groovy's extensions and Nextflow's `Bolts`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use nextflow_parser::Span;

use super::super::{AtSpan, Error, Interpreter, thrown};
use crate::exception::Exception;
use crate::number::{BigDecimal, Number};
use crate::regex::Pattern;
use crate::value::{List, ListKind, Value};

impl Interpreter<'_> {
    pub(super) fn string_method(
        &mut self,
        text: &str,
        name: &str,
        arguments: &[Value],
        span: Span,
    ) -> Result<Option<Value>, Error> {
        let string = |value: &Value| match value {
            Value::String(text) | Value::GString(text) => Some(text.clone()),
            _ => None,
        };
        let integer = |value: &Value| match value {
            Value::Number(number @ (Number::Integer(_) | Number::Long(_))) => Some(number.to_i64()),
            _ => None,
        };
        let length = || text.encode_utf16().count();
        Ok(Some(match (name, arguments) {
            ("size" | "length", []) => Value::integer(length() as i32),
            ("take" | "drop" | "takeRight" | "dropRight", [count]) if integer(count).is_some() => {
                let count = integer(count).unwrap_or(0).max(0) as usize;
                let characters: Vec<char> = text.chars().collect();
                let count = count.min(characters.len());
                let kept: String = match name {
                    "take" => characters[..count].iter().collect(),
                    "drop" => characters[count..].iter().collect(),
                    "takeRight" => characters[characters.len() - count..].iter().collect(),
                    _ => characters[..characters.len() - count].iter().collect(),
                };
                Value::string(kept)
            }
            ("minus", [other]) => Value::string(text)
                .apply(crate::value::Operator::Subtract, other)
                .at(span)?,
            ("isEmpty", []) => Value::Boolean(text.is_empty()),
            ("isBlank", []) => Value::Boolean(text.chars().all(char::is_whitespace)),
            ("toString", []) => Value::string(text),
            ("trim", []) => Value::string(text.trim_matches(|c: char| c <= ' ')),
            ("strip", []) => Value::string(text.trim()),
            ("stripLeading", []) => Value::string(text.trim_start()),
            ("stripTrailing", []) => Value::string(text.trim_end()),
            ("stripIndent", []) => Value::string(strip_indent(text)),
            ("toLowerCase", []) => Value::string(text.to_lowercase()),
            ("toUpperCase", []) => Value::string(text.to_uppercase()),
            ("capitalize", []) => {
                let mut characters = text.chars();
                let first = characters
                    .next()
                    .map(|c| c.to_uppercase().collect::<String>());
                Value::string(format!(
                    "{}{}",
                    first.unwrap_or_default(),
                    characters.as_str()
                ))
            }
            ("reverse", []) => Value::string(text.chars().rev().collect::<String>()),
            ("contains", [other]) if string(other).is_some() => {
                Value::Boolean(text.contains(&*string(other).unwrap_or_default()))
            }
            ("startsWith", [other]) if string(other).is_some() => {
                Value::Boolean(text.starts_with(&*string(other).unwrap_or_default()))
            }
            ("endsWith", [other]) if string(other).is_some() => {
                Value::Boolean(text.ends_with(&*string(other).unwrap_or_default()))
            }
            ("indexOf" | "lastIndexOf", [other, rest @ ..]) if string(other).is_some() => {
                let needle = string(other).unwrap_or_default();
                let from = match rest {
                    [] => None,
                    [from] => Some(integer(from).ok_or_else(|| wrong_arguments(name, span))?),
                    _ => return Ok(None),
                };
                Value::integer(index_of(text, &needle, from, name == "lastIndexOf"))
            }
            ("substring", [begin, rest @ ..]) => {
                let units: Vec<u16> = text.encode_utf16().collect();
                let begin = integer(begin).ok_or_else(|| wrong_arguments(name, span))?;
                let end = match rest {
                    [] => units.len() as i64,
                    [end] => integer(end).ok_or_else(|| wrong_arguments(name, span))?,
                    _ => return Ok(None),
                };
                if begin < 0 || end > units.len() as i64 || begin > end {
                    let message = format!("begin {begin}, end {end}, length {}", units.len());
                    let exception = Exception::new("StringIndexOutOfBoundsException", message);
                    return Err(thrown(exception, span));
                }
                Value::string(String::from_utf16_lossy(
                    &units[begin as usize..end as usize],
                ))
            }
            ("replace", [target, replacement]) if string(target).is_some() => {
                let replacement = string(replacement).ok_or_else(|| wrong_arguments(name, span))?;
                Value::string(text.replace(&*string(target).unwrap_or_default(), &replacement))
            }
            ("replaceAll" | "replaceFirst", [regex, replacement]) => {
                let pattern = pattern(regex, span)?;
                let all = name == "replaceAll";
                match replacement {
                    Value::Closure(closure) => {
                        let closure = closure.clone();
                        let replaced = pattern.replace_with(text, all, |groups| {
                            let argument = match groups.as_slice() {
                                [whole] => text_value(whole),
                                groups => Value::list(groups.iter().map(text_value).collect()),
                            };
                            let result = self.call_closure_value(&closure, vec![argument], span)?;
                            Ok::<_, Error>(result.to_string())
                        })?;
                        Value::string(replaced)
                    }
                    other => {
                        let other = string(other).ok_or_else(|| wrong_arguments(name, span))?;
                        Value::string(pattern.replace(text, &other, all).at(span)?)
                    }
                }
            }
            ("split", [regex, rest @ ..]) => {
                let limit = match rest {
                    [] => 0,
                    [limit] => integer(limit).ok_or_else(|| wrong_arguments(name, span))? as i32,
                    _ => return Ok(None),
                };
                let parts = pattern(regex, span)?.split(text, limit);
                strings(ListKind::Array, parts)
            }
            // Groovy's `split()` gives the words between whitespace, as `tokenize()` does.
            ("split", []) => strings(ListKind::Array, tokens(text, " \t\n\r\u{0C}")),
            ("matches", [regex]) => Value::Boolean(pattern(regex, span)?.matches(text)),
            ("find", [regex]) => match pattern(regex, span)?.all_groups(text).first() {
                Some(groups) => text_value(&groups[0]),
                None => Value::Null,
            },
            ("padLeft" | "padRight" | "center", [width, rest @ ..]) if rest.len() <= 1 => {
                let width = integer(width).ok_or_else(|| wrong_arguments(name, span))? as usize;
                let padding = match rest {
                    [padding] => string(padding).ok_or_else(|| wrong_arguments(name, span))?,
                    _ => " ".into(),
                };
                Value::string(padded(text, name, width, &padding))
            }
            ("tokenize", []) => strings(ListKind::ArrayList, tokens(text, " \t\n\r\u{0C}")),
            ("tokenize", [delimiters]) if string(delimiters).is_some() => {
                let delimiters = string(delimiters).unwrap_or_default();
                strings(ListKind::ArrayList, tokens(text, &delimiters))
            }
            ("readLines", []) => strings(ListKind::ArrayList, lines(text)),
            ("getBytes", []) => super::bytes::bytes(text.as_bytes().to_vec()),
            ("stripMargin", rest) if rest.len() <= 1 => {
                let marker = match rest {
                    [marker] => string(marker)
                        .and_then(|marker| marker.chars().next())
                        .ok_or_else(|| wrong_arguments(name, span))?,
                    _ => '|',
                };
                Value::string(strip_margin(text, marker))
            }
            ("toList", []) => strings(
                ListKind::ArrayList,
                text.chars().map(String::from).collect(),
            ),
            ("count", [other]) if string(other).is_some() => {
                let needle = string(other).unwrap_or_default();
                Value::integer(count(text, &needle))
            }
            (
                "toInteger" | "toLong" | "toBigInteger" | "toBigDecimal" | "toDouble" | "toFloat",
                [],
            ) => parse_number(name, text.trim_matches(|c: char| c <= ' ')).at(span)?,
            (
                "isInteger" | "isLong" | "isBigInteger" | "isBigDecimal" | "isDouble" | "isFloat"
                | "isNumber",
                [],
            ) => {
                let parser = format!("to{}", &name[2..]).replace("toNumber", "toBigDecimal");
                Value::Boolean(parse_number(&parser, text.trim_matches(|c: char| c <= ' ')).is_ok())
            }
            ("toBoolean", []) => {
                let trimmed = text.trim_matches(|c: char| c <= ' ').to_lowercase();
                Value::Boolean(matches!(trimmed.as_str(), "true" | "y" | "1"))
            }
            ("equals", [other]) => Value::Boolean(Value::string(text).java_equals(other)),
            ("equalsIgnoreCase", [other]) => Value::Boolean(
                string(other).is_some_and(|other| other.to_lowercase() == text.to_lowercase()),
            ),
            ("compareTo", [other]) if string(other).is_some() => {
                Value::integer(compare_to(text, &string(other).unwrap_or_default()))
            }
            _ => return Ok(None),
        }))
    }
}

fn text_value(text: &Option<String>) -> Value {
    text.as_deref().map_or(Value::Null, Value::string)
}

fn strings(kind: ListKind, items: Vec<String>) -> Value {
    let items = items.into_iter().map(Value::string).collect();
    Value::List(Rc::new(RefCell::new(List::new(kind, items))))
}

/// A pattern from a string argument or an existing pattern.
fn pattern(value: &Value, span: Span) -> Result<Pattern, Error> {
    match value {
        Value::String(text) | Value::GString(text) => Pattern::new(text).at(span),
        Value::Pattern(pattern) => Ok(Pattern::clone(pattern)),
        _ => Err(wrong_arguments("a regular expression method", span)),
    }
}

fn wrong_arguments(name: &str, span: Span) -> Error {
    let message = format!("No signature of method: java.lang.String.{name}() for these arguments");
    thrown(Exception::new("MissingMethodException", message), span)
}

/// Java's `indexOf` or `lastIndexOf` in UTF-16 code units, or -1.
fn index_of(text: &str, needle: &str, from: Option<i64>, last: bool) -> i32 {
    let units: Vec<u16> = text.encode_utf16().collect();
    let needle: Vec<u16> = needle.encode_utf16().collect();
    let found = |start: usize| units[start..].starts_with(&needle);
    let top = units.len().saturating_sub(needle.len()) as i64;
    if needle.len() > units.len() {
        return -1;
    }
    if last {
        let from = from.unwrap_or(top).min(top);
        (0..=from.max(-1))
            .rev()
            .find(|start| *start >= 0 && found(*start as usize))
            .map_or(-1, |i| i as i32)
    } else {
        let from = from.unwrap_or(0).max(0);
        (from..=top)
            .find(|start| found(*start as usize))
            .map_or(-1, |i| i as i32)
    }
}

/// Groovy's `count`: occurrences that may overlap.
fn count(text: &str, needle: &str) -> i32 {
    if needle.is_empty() {
        return text.encode_utf16().count() as i32 + 1;
    }
    (0..text.len())
        .filter(|start| text.is_char_boundary(*start) && text[*start..].starts_with(needle))
        .count() as i32
}

/// Java's `StringTokenizer`: split on any delimiter character, without empty tokens.
fn tokens(text: &str, delimiters: &str) -> Vec<String> {
    text.split(|c| delimiters.contains(c))
        .filter(|token| !token.is_empty())
        .map(String::from)
        .collect()
}

/// Lines split on `\n`, `\r\n` or `\r`; a final line terminator adds no empty line.
fn lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\r' | '\n' => {
                if character == '\r' && characters.peek() == Some(&'\n') {
                    characters.next();
                }
                lines.push(std::mem::take(&mut current));
            }
            other => current.push(other),
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Java's `String.stripIndent()`: the smallest indent of the non-blank lines and of the last
/// line is removed, trailing spaces go, blank lines become empty.
fn strip_indent(text: &str) -> String {
    let ends_with_terminator = text.ends_with(['\n', '\r']);
    let mut parts = lines(text);
    if ends_with_terminator || text.is_empty() {
        parts.push(String::new());
    }
    let indent = |line: &String| line.chars().take_while(|c| c.is_whitespace()).count();
    let last = parts.len().saturating_sub(1);
    let smallest = parts
        .iter()
        .enumerate()
        .filter(|(index, line)| *index == last || !line.trim().is_empty())
        .map(|(_, line)| indent(line))
        .min()
        .unwrap_or(0);
    let stripped: Vec<String> = parts
        .iter()
        .map(|line| {
            if line.trim().is_empty() {
                String::new()
            } else {
                line.chars()
                    .skip(smallest)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            }
        })
        .collect();
    stripped.join("\n")
}

/// Java's `String.compareTo`: the difference of the first different UTF-16 units, else of the
/// lengths.
fn compare_to(left: &str, right: &str) -> i32 {
    let (left, right): (Vec<u16>, Vec<u16>) = (
        left.encode_utf16().collect(),
        right.encode_utf16().collect(),
    );
    for (a, b) in left.iter().zip(&right) {
        if a != b {
            return i32::from(*a) - i32::from(*b);
        }
    }
    left.len() as i32 - right.len() as i32
}

/// `toInteger()` and the other parse methods, with Java's parsers.
fn parse_number(method: &str, text: &str) -> Result<Value, Exception> {
    let invalid = || Exception::number_format(text);
    let number = match method {
        "toInteger" => Number::Integer(text.parse().map_err(|_| invalid())?),
        "toLong" => Number::Long(text.parse().map_err(|_| invalid())?),
        "toBigInteger" => Number::BigInteger(text.parse().map_err(|_| invalid())?),
        "toBigDecimal" => Number::BigDecimal(BigDecimal::parse(text).ok_or_else(invalid)?),
        "toDouble" => Number::Double(java_float(text).ok_or_else(invalid)?),
        _ => Number::Float(java_float(text).ok_or_else(invalid)? as f32),
    };
    Ok(Value::Number(number))
}

/// Java's `Double.parseDouble`, which also takes a `d` or `f` suffix, `NaN` and `Infinity`.
fn java_float(text: &str) -> Option<f64> {
    let text = text.strip_suffix(['d', 'D', 'f', 'F']).unwrap_or(text);
    match text.trim_start_matches(['+', '-']) {
        "NaN" | "Infinity" => text.replace("Infinity", "inf").parse().ok(),
        "inf" | "infinity" | "nan" => None,
        _ => text.parse().ok(),
    }
}

/// Groovy's `padLeft`, `padRight` and `center`: the text with the padding repeated around it
/// up to `width` characters.
fn padded(text: &str, name: &str, width: usize, padding: &str) -> String {
    let length = text.chars().count();
    if width <= length || padding.is_empty() {
        return text.to_owned();
    }
    let fill = |count: usize| padding.chars().cycle().take(count).collect::<String>();
    let missing = width - length;
    match name {
        "padLeft" => format!("{}{text}", fill(missing)),
        "padRight" => format!("{text}{}", fill(missing)),
        _ => {
            let half = fill(missing / 2 + missing % 2);
            let left: String = half.chars().take(missing / 2).collect();
            format!("{left}{text}{half}")
        }
    }
}

/// Groovy's `stripMargin`: each line without the blanks and the marker it starts with, if it
/// does. Line ends become `\n`.
fn strip_margin(text: &str, marker: char) -> String {
    let mut out = String::new();
    for line in lines(text) {
        let rest = line.trim_start_matches(|c: char| c <= ' ');
        out.push_str(rest.strip_prefix(marker).unwrap_or(&line));
        out.push('\n');
    }
    if !text.ends_with('\n') {
        out.pop();
    }
    out
}
