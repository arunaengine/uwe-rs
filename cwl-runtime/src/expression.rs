//! Evaluates CWL parameter references, interpolation and enabled JavaScript expressions.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::Arc;

use cwl_parser::model::{Expression, ExpressionKind, Part};

use crate::javascript::helper::{FRAME_LIMIT, Failure, JavaScriptSettings};
use crate::value::Value;

/// What an expression sees: `inputs`, `self` and `runtime`.
#[derive(Clone, Copy, Debug)]
pub struct Variables<'a> {
    pub inputs: &'a Value,
    pub this: &'a Value,
    pub runtime: &'a Value,
}

/// Evaluates the expressions of one process.
///
/// Without `InlineJavascriptRequirement` the spec allows parameter references only and says
/// that nothing else is interpolated; cwltool fails on anything else. Documents written for
/// other runners use JavaScript without the requirement, and bash variables like `${HOME}` in
/// scripts. So without it, `$(...)` that is not a parameter reference runs as JavaScript,
/// since it cannot mean text in a command, and `${...}` that does not run as JavaScript stays
/// as written.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Evaluator {
    /// The `expressionLib` of `InlineJavascriptRequirement`; `None` when the requirement is
    /// not in effect.
    pub javascript: Option<Arc<Vec<String>>>,
    pub settings: JavaScriptSettings,
}

impl Evaluator {
    pub fn with_javascript(library: Vec<String>) -> Self {
        Evaluator {
            javascript: Some(Arc::new(library)),
            ..Evaluator::default()
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
                // Without the requirement only real references fail here; `$(1)` or `$(true)`
                // are not references and run as JavaScript.
                Err(error) if self.javascript.is_none() && refers(code) => Err(error),
                Err(_) => self
                    .javascript_expression(code, variables)
                    .map_err(Failure::text),
            },
            Part::Javascript(code) => self
                .javascript_expression(code, variables)
                .map_err(Failure::text),
            Part::Function(body) => match self.javascript(body, body, variables) {
                Err(error) if self.javascript.is_none() && error.language() => {
                    Ok(Value::String(format!("${{{body}}}")))
                }
                result => result.map_err(Failure::text),
            },
        }
    }

    fn javascript_expression(
        &self,
        code: &str,
        variables: Variables<'_>,
    ) -> Result<Value, Failure> {
        if code.len() > FRAME_LIMIT {
            return Err(Failure::Boundary(
                "expression source exceeded 16 MiB".into(),
            ));
        }
        self.javascript(&format!("return ({code}\n);"), code, variables)
    }

    /// Runs a function body; `code` is the expression as written, for messages.
    fn javascript(
        &self,
        body: &str,
        code: &str,
        variables: Variables<'_>,
    ) -> Result<Value, Failure> {
        let library = self.javascript.as_deref().map_or(&[][..], Vec::as_slice);
        let globals = [
            ("inputs", variables.inputs),
            ("self", variables.this),
            ("runtime", variables.runtime),
        ];
        self.settings
            .evaluate(library, &globals, body)
            .map_err(|error| {
                let Failure::Language(error) = error else {
                    return error;
                };
                let error = error.lines().next().unwrap_or_default().to_owned();
                let note = if self.javascript.is_none() {
                    " (the process has no InlineJavascriptRequirement)"
                } else {
                    ""
                };
                Failure::Language(format!(
                    "JavaScript error in `{}`: {error}{note}",
                    shown(code)
                ))
            })
    }
}

/// Whether a reference starts with `inputs`, `self` or `runtime`.
fn refers(code: &str) -> bool {
    let end = code
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(code.len());
    matches!(&code[..end], "inputs" | "self" | "runtime")
}

/// Code for messages: on one line, and cut after 80 characters.
fn shown(code: &str) -> String {
    let mut line = String::new();
    let mut length = 0;
    for word in code.split_whitespace() {
        for character in (!line.is_empty())
            .then_some(' ')
            .into_iter()
            .chain(word.chars())
        {
            if length == 80 {
                line.push_str("...");
                return line;
            }
            line.push(character);
            length += 1;
        }
    }
    line
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
                current = map.get(&key).ok_or_else(|| match path.as_str() {
                    "inputs" => format!("there is no input `{key}`"),
                    _ => format!("`{path}` has no `{key}`"),
                })?;
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
