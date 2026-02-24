//! CWL expressions and parameter references, kept as text and split into their parts.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

/// A string that may contain `$(...)` or `${...}`, with the parts an evaluator needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expression {
    /// The text as written in the document.
    pub text: String,
    /// The text split into literal text and code; it is empty for an empty string.
    pub parts: Vec<Part>,
}

/// One piece of an expression string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    /// Literal text, with escapes already applied.
    Text(String),
    /// A parameter reference such as `inputs.reads.path`, which needs no JavaScript.
    Reference(String),
    /// The JavaScript expression inside `$(...)`.
    Javascript(String),
    /// The JavaScript function body inside `${...}`.
    Function(String),
}

/// A setting given as a value or as an expression that computes it at run time.
#[derive(Clone, Debug, PartialEq)]
pub enum Dynamic<T> {
    Value(T),
    Expression(Expression),
}

/// What an expression string is as a whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpressionKind {
    /// Plain text without code.
    Literal,
    /// A single parameter reference; its value keeps its type.
    Reference,
    /// A single JavaScript expression or function body; its value keeps its type.
    Javascript,
    /// Text mixed with code; the value is a string.
    Interpolation,
}

impl Expression {
    /// Splits a string into parts as cwl-utils does when it evaluates one.
    ///
    /// `old_escapes` selects the backslash rule of CWL v1.0 up to v1.2.0-dev3.
    pub fn parse(text: &str, old_escapes: bool) -> Result<Self, String> {
        if !text.contains("$(") && !text.contains("${") {
            let parts = if text.is_empty() {
                Vec::new()
            } else {
                vec![Part::Text(text.to_owned())]
            };
            return Ok(Self {
                text: text.to_owned(),
                parts,
            });
        }
        // Evaluation strips surrounding white space before scanning, so a trailing newline
        // from a YAML block does not turn a single expression into a string.
        let mut rest = text.trim();
        let mut parts = Vec::new();
        let mut literal = String::new();
        while let Some((start, mut end)) = scan(rest)? {
            literal.push_str(&rest[..start]);
            let piece = &rest[start..end];
            if let Some(code) = piece.strip_prefix("$(") {
                let code = &code[..code.len() - 1];
                flush(&mut literal, &mut parts);
                if is_reference(code) {
                    parts.push(Part::Reference(code.to_owned()));
                } else {
                    parts.push(Part::Javascript(code.to_owned()));
                }
            } else if let Some(code) = piece.strip_prefix("${") {
                flush(&mut literal, &mut parts);
                parts.push(Part::Function(code[..code.len() - 1].to_owned()));
            } else if old_escapes {
                literal.push_str(&piece[1..]);
            } else {
                let lookahead = rest.get(start..end + 1).unwrap_or(piece);
                if lookahead == "\\$(" || lookahead == "\\${" {
                    literal.push_str(&lookahead[1..]);
                    end += 1;
                } else if piece == "\\\\" {
                    literal.push('\\');
                } else {
                    literal.push_str(piece);
                }
            }
            rest = &rest[end..];
        }
        literal.push_str(rest);
        flush(&mut literal, &mut parts);
        Ok(Self {
            text: text.to_owned(),
            parts,
        })
    }

    pub fn kind(&self) -> ExpressionKind {
        match self.parts.as_slice() {
            [Part::Reference(_)] => ExpressionKind::Reference,
            [Part::Javascript(_) | Part::Function(_)] => ExpressionKind::Javascript,
            parts if parts.iter().all(|part| matches!(part, Part::Text(_))) => {
                ExpressionKind::Literal
            }
            _ => ExpressionKind::Interpolation,
        }
    }

    /// Whether evaluating this needs a JavaScript engine.
    pub fn uses_javascript(&self) -> bool {
        let code = |part: &Part| matches!(part, Part::Javascript(_) | Part::Function(_));
        self.parts.iter().any(code)
    }
}

fn flush(literal: &mut String, parts: &mut Vec<Part>) {
    if !literal.is_empty() {
        parts.push(Part::Text(std::mem::take(literal)));
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Plain,
    Dollar,
    Paren,
    Brace,
    Single,
    Double,
    Backslash,
}

/// The byte range of the next `$(...)`, `${...}` or backslash escape, as cwl-utils'
/// `scanner` finds it: brackets nest, and brackets inside quoted strings do not count.
fn scan(text: &str) -> Result<Option<(usize, usize)>, String> {
    let bytes = text.as_bytes();
    let mut stack = vec![State::Plain];
    let mut start = 0;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let state = *stack.last().unwrap_or(&State::Plain);
        match (state, byte) {
            (State::Plain, b'$') => stack.push(State::Dollar),
            (State::Plain, b'\\') => stack.push(State::Backslash),
            (State::Backslash, _) => {
                stack.pop();
                if stack.last() == Some(&State::Plain) {
                    return Ok(Some((index - 1, index + 1)));
                }
            }
            (State::Dollar, b'(' | b'{') => {
                start = index - 1;
                stack.push(if byte == b'(' {
                    State::Paren
                } else {
                    State::Brace
                });
            }
            (State::Dollar, _) => {
                stack.pop();
                continue;
            }
            (State::Paren, b'(') => stack.push(State::Paren),
            (State::Brace, b'{') => stack.push(State::Brace),
            (State::Paren, b')') | (State::Brace, b'}') => {
                stack.pop();
                if stack.last() == Some(&State::Dollar) {
                    return Ok(Some((start, index + 1)));
                }
            }
            (State::Paren | State::Brace, b'\'') => stack.push(State::Single),
            (State::Paren | State::Brace, b'"') => stack.push(State::Double),
            (State::Single, b'\'') | (State::Double, b'"') => {
                stack.pop();
            }
            (State::Single | State::Double, b'\\') => stack.push(State::Backslash),
            _ => {}
        }
        index += 1;
    }
    let trailing = matches!(stack.as_slice(), [_, State::Backslash | State::Dollar]);
    if stack.len() > 1 && !trailing {
        return Err(format!("unfinished expression at `{}`", &text[start..]));
    }
    Ok(None)
}

/// Whether code matches the parameter reference grammar: a symbol followed by `.symbol`,
/// `['text']`, `["text"]` or `[index]` segments.
fn is_reference(code: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let symbol_end = code.find(|c: char| !word(c)).unwrap_or(code.len());
    if symbol_end == 0 {
        return false;
    }
    let mut rest = &code[symbol_end..];
    while !rest.is_empty() {
        let after = if let Some(tail) = rest.strip_prefix('.') {
            let end = tail.find(|c: char| !word(c)).unwrap_or(tail.len());
            (end > 0).then(|| &tail[end..])
        } else if let Some(tail) = rest.strip_prefix("['") {
            quoted_end(tail, '\'')
        } else if let Some(tail) = rest.strip_prefix("[\"") {
            quoted_end(tail, '"')
        } else if let Some(tail) = rest.strip_prefix('[') {
            let end = tail
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(tail.len());
            (end > 0).then(|| tail[end..].strip_prefix(']')).flatten()
        } else {
            None
        };
        match after {
            Some(after) => rest = after,
            None => return false,
        }
    }
    true
}

/// The text after a non-empty quoted segment and its closing `]`.
fn quoted_end(text: &str, quote: char) -> Option<&str> {
    let mut chars = text.char_indices();
    let mut length = 0;
    while let Some((index, c)) = chars.next() {
        if c == '\\' && text[index + 1..].starts_with(quote) {
            chars.next();
            length += 2;
        } else if c == quote {
            let after = &text[index + 1..];
            return (length > 0).then(|| after.strip_prefix(']')).flatten();
        } else {
            length += 1;
        }
    }
    None
}

#[cfg(test)]
#[path = "expression_tests.rs"]
mod tests;
