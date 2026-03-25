//! Evaluates CWL expressions: parameter references natively, JavaScript in the embedded
//! engine when `InlineJavascriptRequirement` is in effect, and string interpolation as
//! cwltool does it.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::Arc;

use cwl_parser::model::{Expression, ExpressionKind, Part};

use crate::javascript;
use crate::value::Value;

/// What an expression sees: `inputs`, `self` and `runtime`.
#[derive(Clone, Copy, Debug)]
pub struct Variables<'a> {
    pub inputs: &'a Value,
    pub this: &'a Value,
    pub runtime: &'a Value,
}

/// Evaluates the expressions of one process.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Evaluator {
    /// The `expressionLib` when JavaScript is enabled; `None` allows parameter references only.
    pub javascript: Option<Arc<Vec<String>>>,
}

impl Evaluator {
    pub fn with_javascript(library: Vec<String>) -> Self {
        Evaluator {
            javascript: Some(Arc::new(library)),
        }
    }

    /// The value of an expression: a single reference or code keeps its type, text mixed with
    /// code becomes a string, and text alone stays as written.
    pub fn evaluate(
        &self,
        expression: &Expression,
        variables: Variables<'_>,
    ) -> Result<Value, String> {
        self.parts(&expression.parts, expression.kind(), variables)
    }

    /// Like [`Evaluator::evaluate`], but white space around a single expression stays and
    /// makes the result a string, as cwltool evaluates `Dirent.entry`.
    pub fn evaluate_untrimmed(
        &self,
        expression: &Expression,
        variables: Variables<'_>,
    ) -> Result<Value, String> {
        let text = &expression.text;
        let has_code = expression
            .parts
            .iter()
            .any(|part| !matches!(part, Part::Text(_)));
        let leading = &text[..text.len() - text.trim_start().len()];
        let trailing = &text[text.trim_end().len()..];
        if !has_code || (leading.is_empty() && trailing.is_empty()) {
            return self.evaluate(expression, variables);
        }
        let mut parts = Vec::with_capacity(expression.parts.len() + 2);
        parts.push(Part::Text(leading.to_owned()));
        parts.extend(expression.parts.iter().cloned());
        parts.push(Part::Text(trailing.to_owned()));
        parts.retain(|part| !matches!(part, Part::Text(text) if text.is_empty()));
        self.parts(&parts, ExpressionKind::Interpolation, variables)
    }

    fn parts(
        &self,
        parts: &[Part],
        kind: ExpressionKind,
        variables: Variables<'_>,
    ) -> Result<Value, String> {
        match (kind, parts) {
            (ExpressionKind::Reference | ExpressionKind::Javascript, [part]) => {
                self.part(part, variables)
            }
            _ => {
                let mut text = String::new();
                for part in parts {
                    match part {
                        Part::Text(literal) => text.push_str(literal),
                        code => match self.part(code, variables)? {
                            Value::String(value) => text.push_str(&value),
                            value => text.push_str(&value.to_python_json(true)),
                        },
                    }
                }
                Ok(Value::String(text))
            }
        }
    }

    fn part(&self, part: &Part, variables: Variables<'_>) -> Result<Value, String> {
        match part {
            Part::Text(text) => Ok(Value::String(text.clone())),
            Part::Reference(code) => match reference(code, variables) {
                Ok(value) => Ok(value),
                Err(error) if self.javascript.is_none() => Err(error),
                Err(_) => self.javascript(&format!("return ({code}\n);"), code, variables),
            },
            Part::Javascript(code) => {
                self.javascript(&format!("return ({code}\n);"), code, variables)
            }
            Part::Function(body) => self.javascript(body, body, variables),
        }
    }

    /// Runs a function body; `code` is the expression as written, for messages.
    fn javascript(
        &self,
        body: &str,
        code: &str,
        variables: Variables<'_>,
    ) -> Result<Value, String> {
        let Some(library) = &self.javascript else {
            return Err(format!(
                "`{}` is JavaScript, which needs InlineJavascriptRequirement",
                code.trim()
            ));
        };
        let globals = [
            ("inputs", variables.inputs),
            ("self", variables.this),
            ("runtime", variables.runtime),
        ];
        javascript::evaluate(library, &globals, body)
            .map_err(|error| format!("JavaScript error in `{}`: {error}", code.trim()))
    }
}

/// Evaluates a parameter reference such as `inputs.reads[0].path`, following cwl-utils:
/// `.length` of a list is its length, and a missing key or index is an error.
pub fn reference(code: &str, variables: Variables<'_>) -> Result<Value, String> {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let end = code.find(|c: char| !word(c)).unwrap_or(code.len());
    let (symbol, mut rest) = code.split_at(end);
    if symbol == "null" && rest.is_empty() {
        return Ok(Value::Null);
    }
    let mut current = match symbol {
        "inputs" => variables.inputs,
        "self" => variables.this,
        "runtime" => variables.runtime,
        other => return Err(format!("`{other}` is not defined")),
    };
    let mut path = symbol.to_owned();
    while !rest.is_empty() {
        let (segment, after) = next_segment(rest).ok_or_else(|| format!("cannot read `{code}`"))?;
        match segment {
            Segment::Key(key) => {
                if key == "length"
                    && after.is_empty()
                    && let Value::List(items) = current
                {
                    return Ok(Value::Integer(items.len() as i64));
                }
                let Value::Map(map) = current else {
                    return Err(format!(
                        "`{path}` is {}, which has no `{key}`",
                        kind(current)
                    ));
                };
                current = map
                    .get(&key)
                    .ok_or_else(|| format!("`{path}` does not contain the key `{key}`"))?;
                path.push('.');
                path.push_str(&key);
            }
            Segment::Index(index) => {
                let Value::List(items) = current else {
                    return Err(format!("`{path}` is {}, not a list", kind(current)));
                };
                current = items
                    .get(index)
                    .ok_or_else(|| format!("`{path}` has no index {index}"))?;
                path.push_str(&format!("[{index}]"));
            }
        }
        rest = after;
    }
    Ok(current.clone())
}

enum Segment {
    Key(String),
    Index(usize),
}

/// The next `.name`, `['name']`, `["name"]` or `[index]` segment and the text after it.
fn next_segment(text: &str) -> Option<(Segment, &str)> {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    if let Some(tail) = text.strip_prefix('.') {
        let end = tail.find(|c: char| !word(c)).unwrap_or(tail.len());
        return (end > 0).then(|| (Segment::Key(tail[..end].to_owned()), &tail[end..]));
    }
    let tail = text.strip_prefix('[')?;
    if let Some(quote) = tail.chars().next().filter(|c| *c == '\'' || *c == '"') {
        let body = &tail[1..];
        let mut key = String::new();
        let mut characters = body.char_indices();
        while let Some((index, c)) = characters.next() {
            if c == '\\' && body[index + 1..].starts_with(quote) {
                key.push(quote);
                characters.next();
            } else if c == quote {
                let after = body[index + 1..].strip_prefix(']')?;
                return Some((Segment::Key(key), after));
            } else {
                key.push(c);
            }
        }
        return None;
    }
    let end = tail.find(']')?;
    let index = tail[..end].parse().ok()?;
    Some((Segment::Index(index), &tail[end + 1..]))
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Integer(_) | Value::BigInteger(_) | Value::Float(_) => "a number",
        Value::String(_) => "a string",
        Value::List(_) => "a list",
        Value::Map(_) => "a map",
    }
}

#[cfg(test)]
#[path = "expression_tests.rs"]
mod tests;
