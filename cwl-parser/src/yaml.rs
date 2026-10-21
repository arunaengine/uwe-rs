//! Reads YAML and JSON text into nodes, keeping the location of every value.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use granit_parser::{Event, Parser, ScalarStyle, ScanError, Span, Tag, options};

use crate::error::Error;
use crate::node::{Entry, Location, Node, Value};

/// The message of the parser for a continuation line of a quoted scalar that is not indented
/// more than the key of the scalar.
const QUOTED_INDENTATION: &str = "invalid indentation in multiline quoted scalar";

/// The message of the parser for a lone `-` in a flow collection, like `[cat, -]`.
const LONE_DASH: &str = "plain scalar cannot start with '-'";

/// Aliases may copy at most this many values in one document, which stops alias bombs.
const ALIAS_BUDGET: usize = 1_000_000;

/// Deeper nesting is refused, so recursion over trees cannot overflow the stack.
const DEPTH_LIMIT: usize = 1_000;

/// Reads the first document of a YAML stream, resolving scalars with the YAML 1.2 core schema.
pub fn read(text: &str, file: &Arc<str>) -> Result<Node, Error> {
    let mut text = std::borrow::Cow::Borrowed(text);
    for _ in 0..256 {
        let error = match read_once(&text, file) {
            Err(error) if error.location.line > 0 => error,
            result => return result,
        };
        let line = error.location.line as usize - 1;
        let column = error.location.column as usize - 1;
        let mut lines: Vec<&str> = text.split('\n').collect();
        let Some(found) = lines.get(line) else {
            return Err(error);
        };
        let changed = if error.message == QUOTED_INDENTATION {
            format!(" {found}")
        } else if error.message.starts_with(LONE_DASH)
            && found.get(column..column + 1) == Some("-")
            && found[column + 1..].starts_with([',', ']', '}', ' '])
        {
            format!("{}\"-\"{}", &found[..column], &found[column + 1..])
        } else {
            return Err(error);
        };
        lines[line] = &changed;
        text = std::borrow::Cow::Owned(lines.join("\n"));
    }
    read_once(&text, file)
}

fn read_once(text: &str, file: &Arc<str>) -> Result<Node, Error> {
    let mut parser = Parser::new_from_str_with_options(text, options! { emit_comments: false });
    let mut builder = Builder {
        file,
        stack: Vec::new(),
        anchors: HashMap::new(),
        aliased: aliased(text),
        budget: ALIAS_BUDGET,
        root: None,
    };
    while let Some(next) = parser.next_event() {
        let (event, span) = next.map_err(|error| scan_error(&error, file))?;
        let location = builder.location(&span);
        match event {
            Event::Scalar(text, style, anchor, tag) => {
                builder.scalar(&text, style, anchor, tag.as_deref(), location)?;
            }
            Event::SequenceStart(_, anchor, tag) => {
                builder.open(Value::List(Vec::new()), anchor, tag.as_deref(), location)?;
            }
            Event::MappingStart(_, anchor, tag) => {
                builder.open(Value::Map(Vec::new()), anchor, tag.as_deref(), location)?;
            }
            Event::SequenceEnd | Event::MappingEnd => builder.close()?,
            Event::Alias(anchor) => builder.alias(anchor, location)?,
            Event::DocumentEnd => break,
            _ => {}
        }
    }
    let empty = Node::new(Value::Null, Location::new(file, 1, 1));
    Ok(builder.root.unwrap_or(empty))
}

/// The anchors that aliases of the first document name. Only these are copied, so anchors
/// that no alias names cost nothing.
fn aliased(text: &str) -> HashSet<usize> {
    let mut parser = Parser::new_from_str_with_options(text, options! { emit_comments: false });
    let mut found = HashSet::new();
    while let Some(Ok((event, _))) = parser.next_event() {
        match event {
            Event::Alias(anchor) => {
                found.insert(anchor);
            }
            Event::DocumentEnd => break,
            _ => {}
        }
    }
    found
}

/// A list or map under construction, with the key that waits for its value, and the count
/// of values and the depth of what it holds so far.
struct Frame {
    node: Node,
    anchor: usize,
    key: Option<(String, Location)>,
    size: Size,
}

/// The count of values in a node and how deep it nests, kept while building so that each node
/// is counted once.
#[derive(Clone, Copy)]
struct Size {
    values: usize,
    depth: usize,
}

struct Builder<'a> {
    file: &'a Arc<str>,
    stack: Vec<Frame>,
    /// Each anchored node that an alias names, with its count of values and its depth.
    anchors: HashMap<usize, (Node, Size)>,
    aliased: HashSet<usize>,
    /// How many more values aliases may copy.
    budget: usize,
    root: Option<Node>,
}

impl Builder<'_> {
    fn location(&self, span: &Span) -> Location {
        let line = span.start.line() as u32;
        Location::new(self.file, line, span.start.col() as u32 + 1)
    }

    fn expects_key(&self) -> bool {
        match self.stack.last() {
            Some(frame) => matches!(frame.node.value, Value::Map(_)) && frame.key.is_none(),
            None => false,
        }
    }

    fn scalar(
        &mut self,
        text: &str,
        style: ScalarStyle,
        anchor: usize,
        tag: Option<&Tag>,
        location: Location,
    ) -> Result<(), Error> {
        if self.expects_key() {
            if style == ScalarStyle::Plain && tag.is_none() && plain_value(text) == Value::Null {
                return Err(Error::new(&location, "a map key must not be null"));
            }
            if let Some(frame) = self.stack.last_mut() {
                frame.key = Some((text.to_owned(), location));
            }
            return Ok(());
        }
        let value = match tag {
            Some(tag) => tagged_value(text, tag, &location)?,
            None if style == ScalarStyle::Plain => plain_value(text),
            None => Value::String(text.to_owned()),
        };
        let node = Node::new(value, location);
        let size = Size {
            values: 1,
            depth: 0,
        };
        self.remember(anchor, &node, size)?;
        self.push(node, size)
    }

    fn open(
        &mut self,
        value: Value,
        anchor: usize,
        tag: Option<&Tag>,
        location: Location,
    ) -> Result<(), Error> {
        if self.expects_key() {
            return Err(Error::new(&location, "a map key must be a string"));
        }
        if self.stack.len() >= DEPTH_LIMIT {
            let message = format!("values are nested more than {DEPTH_LIMIT} levels deep");
            return Err(Error::new(&location, message));
        }
        if let Some(tag) = tag {
            let expected = if matches!(value, Value::Map(_)) {
                "map"
            } else {
                "seq"
            };
            if tag.core_suffix() != Some(expected) {
                return Err(Error::new(&location, format!("unsupported YAML tag {tag}")));
            }
        }
        let node = Node::new(value, location);
        self.stack.push(Frame {
            node,
            anchor,
            key: None,
            size: Size {
                values: 1,
                depth: 1,
            },
        });
        Ok(())
    }

    fn close(&mut self) -> Result<(), Error> {
        let Some(frame) = self.stack.pop() else {
            return Ok(());
        };
        self.remember(frame.anchor, &frame.node, frame.size)?;
        self.push(frame.node, frame.size)
    }

    fn alias(&mut self, anchor: usize, location: Location) -> Result<(), Error> {
        if self.expects_key() {
            return Err(Error::new(&location, "a map key must not be an alias"));
        }
        let Some((node, size)) = self.anchors.get(&anchor) else {
            return Err(Error::new(&location, "alias to an unknown anchor"));
        };
        let size = *size;
        if size.values > self.budget {
            return Err(Error::new(&location, "aliases expand to too many values"));
        }
        if self.stack.len() + size.depth > DEPTH_LIMIT {
            let message = format!("values are nested more than {DEPTH_LIMIT} levels deep");
            return Err(Error::new(&location, message));
        }
        self.budget -= size.values;
        let node = node.clone();
        self.push(node, size)
    }

    /// Keeps a copy of an anchored node that an alias names, which counts as a copy of aliases.
    fn remember(&mut self, anchor: usize, node: &Node, size: Size) -> Result<(), Error> {
        if anchor == 0 || !self.aliased.contains(&anchor) {
            return Ok(());
        }
        if size.values > self.budget {
            return Err(Error::new(
                &node.location,
                "aliases expand to too many values",
            ));
        }
        self.budget -= size.values;
        self.anchors.insert(anchor, (node.clone(), size));
        Ok(())
    }

    fn push(&mut self, node: Node, size: Size) -> Result<(), Error> {
        let Some(frame) = self.stack.last_mut() else {
            self.root.get_or_insert(node);
            return Ok(());
        };
        let counted = Size {
            values: frame.size.values + size.values,
            depth: frame.size.depth.max(size.depth + 1),
        };
        match &mut frame.node.value {
            Value::List(items) => {
                items.push(node);
                frame.size = counted;
            }
            Value::Map(entries) => {
                let Some((key, location)) = frame.key.take() else {
                    return Ok(());
                };
                if entries.iter().any(|entry| entry.key == key) {
                    return Err(Error::new(&location, format!("duplicate key `{key}`")));
                }
                entries.push(Entry {
                    key,
                    location,
                    value: node,
                });
                frame.size = counted;
            }
            _ => {}
        }
        Ok(())
    }
}

fn scan_error(error: &ScanError, file: &Arc<str>) -> Error {
    let marker = error.marker();
    let location = Location::new(file, marker.line() as u32, marker.col() as u32 + 1);
    Error::new(&location, error.info())
}

/// Applies a core schema tag such as `!!str` to a scalar.
fn tagged_value(text: &str, tag: &Tag, location: &Location) -> Result<Value, Error> {
    let resolved = plain_value(text);
    let value = match tag.core_suffix() {
        Some("str") => Some(Value::String(text.to_owned())),
        Some("null") => (resolved == Value::Null).then_some(Value::Null),
        Some("bool") => matches!(resolved, Value::Bool(_)).then_some(resolved),
        Some("int") => {
            matches!(resolved, Value::Integer(_) | Value::BigInteger(_)).then_some(resolved)
        }
        Some("float") => match resolved {
            Value::Integer(number) => Some(Value::Float(number as f64)),
            Value::BigInteger(digits) => digits.parse().ok().map(Value::Float),
            Value::Float(_) => Some(resolved),
            _ => None,
        },
        _ if tag.handle() == "!" && tag.suffix().is_empty() => Some(Value::String(text.into())),
        _ => return Err(Error::new(location, format!("unsupported YAML tag {tag}"))),
    };
    value.ok_or_else(|| Error::new(location, format!("`{text}` does not fit the tag {tag}")))
}

/// Resolves an untagged plain scalar as the YAML 1.2 core schema does.
fn plain_value(text: &str) -> Value {
    match text {
        "" | "~" | "null" | "Null" | "NULL" => Value::Null,
        "true" | "True" | "TRUE" => Value::Bool(true),
        "false" | "False" | "FALSE" => Value::Bool(false),
        ".nan" | ".NaN" | ".NAN" => Value::Float(f64::NAN),
        _ => integer(text)
            .or_else(|| float(text))
            .unwrap_or_else(|| Value::String(text.to_owned())),
    }
}

fn integer(text: &str) -> Option<Value> {
    if let Some(octal) = text.strip_prefix("0o") {
        return i64::from_str_radix(octal, 8).ok().map(Value::Integer);
    }
    if let Some(hex) = text.strip_prefix("0x") {
        return i64::from_str_radix(hex, 16).ok().map(Value::Integer);
    }
    let digits = text.strip_prefix(['-', '+']).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    match text.parse::<i64>() {
        Ok(number) => Some(Value::Integer(number)),
        Err(_) => {
            // Larger integers keep their digits, as they do in Python and in JSON.
            let digits = digits.trim_start_matches('0');
            let sign = if text.starts_with('-') { "-" } else { "" };
            Some(Value::BigInteger(format!("{sign}{digits}")))
        }
    }
}

fn float(text: &str) -> Option<Value> {
    let unsigned = text.strip_prefix(['-', '+']).unwrap_or(text);
    if matches!(unsigned, ".inf" | ".Inf" | ".INF") {
        let infinity = if text.starts_with('-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
        return Some(Value::Float(infinity));
    }
    let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, Some(exponent)),
        None => (unsigned, None),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    let valid_mantissa = if mantissa.contains('.') {
        digits(whole) && digits(fraction) && !(whole.is_empty() && fraction.is_empty())
    } else {
        !whole.is_empty() && digits(whole) && exponent.is_some()
    };
    let valid_exponent = exponent.is_none_or(|exponent| {
        let exponent = exponent.strip_prefix(['-', '+']).unwrap_or(exponent);
        !exponent.is_empty() && digits(exponent)
    });
    if !(valid_mantissa && valid_exponent) {
        return None;
    }
    text.parse::<f64>().ok().map(Value::Float)
}

#[cfg(test)]
#[path = "yaml_tests.rs"]
mod tests;
