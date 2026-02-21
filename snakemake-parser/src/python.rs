//! Parses the Python parts of a Snakefile with ruff, keeping their offsets in the Snakefile.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use ruff_python_ast::{Arguments, AtomicNodeIndex, Expr, ExprCall, Stmt, Suite};
use ruff_python_parser::{ParseError, parse_expression_range, parse_module};
use ruff_text_size::{TextRange, TextSize};

use crate::Error;

/// Where parser.py puts a value in its generated Python, which decides what the value may hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Form {
    /// `f(<value>)`, as for rule properties and global keywords.
    Plain,
    /// `f(<value>, k=0)`: the generated `shell(...)` call adds keyword arguments.
    Shell,
    /// `f(<value>, 0)`: `script(...)` and similar calls add positional arguments.
    Command,
    /// `f(k=0, <value>)`: storage values follow the tag keyword.
    Storage,
}

/// Parses a value as the arguments of a call; `None` stands for an empty value after `offset`.
pub(crate) fn arguments(
    source: &str,
    value: Option<TextRange>,
    offset: TextSize,
    form: Form,
) -> Result<Arguments, Error> {
    let Some(range) = value else {
        return Ok(empty(offset));
    };
    let (before, after) = match form {
        Form::Plain => ("f(", ")"),
        Form::Shell => ("f(", ",k=0)"),
        Form::Command => ("f(", ",0)"),
        Form::Storage => ("f(k=0,", ")"),
    };
    let mut arguments = call(source, range, before, after)?.arguments;
    match form {
        Form::Plain => {}
        Form::Shell => {
            arguments.keywords.pop();
        }
        Form::Command => {
            arguments.args.pop();
        }
        Form::Storage => {
            arguments.keywords.remove(0);
        }
    }
    arguments.range = range;
    Ok(arguments)
}

/// An empty argument list at `offset`.
pub(crate) fn empty(offset: TextSize) -> Arguments {
    Arguments {
        node_index: AtomicNodeIndex::NONE,
        range: TextRange::empty(offset),
        args: Default::default(),
        keywords: Default::default(),
    }
}

/// Parses a module setting, which parser.py passes on as one keyword argument.
pub(crate) fn expression(source: &str, range: TextRange) -> Result<Expr, Error> {
    let arguments = call(source, range, "f(k=", ",j=0)")?.arguments;
    if arguments.args.is_empty()
        && arguments.keywords.len() == 2
        && let Some(keyword) = arguments.keywords.into_iter().next()
    {
        return Ok(keyword.value);
    }
    Err(Error::new(source, "Expected a single value", range))
}

/// Parses one string token, such as a rule docstring.
pub(crate) fn string(source: &str, range: TextRange) -> Result<Expr, Error> {
    let parsed =
        parse_expression_range(source, range).map_err(|error| located(source, &error, range))?;
    Ok(parsed.into_expr())
}

/// Parses the statements of a `run:` block or a handler, from its first to its last token.
pub(crate) fn body(source: &str, range: TextRange) -> Result<Suite, Error> {
    let start = range.start().to_usize();
    let line_start = source[..start]
        .rfind(['\n', '\r'])
        .map_or(0, |index| index + 1);
    let inline = !source[line_start..start].trim_start().is_empty();
    let (header, from) = if inline {
        ("if 1:", start)
    } else {
        ("if 1:\n", line_start)
    };
    // A comment line pads the text so that the body keeps its offsets.
    let Some(fill) = from.checked_sub(header.len() + 2) else {
        return Err(Error::new(source, "Expected a statement", range));
    };
    let mut text = String::with_capacity(range.end().to_usize() + 1);
    text.push('#');
    text.extend(std::iter::repeat_n(' ', fill));
    text.push('\n');
    text.push_str(header);
    text.push_str(&source[from..range.end().to_usize()]);
    text.push('\n');
    let parsed = parse_module(&text).map_err(|error| located(source, &error, range))?;
    let Some(Stmt::If(statement)) = parsed.into_syntax().body.pop() else {
        return Err(Error::new(source, "Expected a statement", range));
    };
    Ok(statement.body)
}

/// Parses the Python code around the Snakemake statements, which become `...` placeholders.
pub(crate) fn module(source: &str, statements: &[TextRange]) -> Result<Suite, Error> {
    let mut bytes = source.as_bytes().to_vec();
    for range in statements {
        let region = &mut bytes[range.start().to_usize()..range.end().to_usize()];
        // Every statement starts with an ASCII keyword of at least three letters.
        region[..3].copy_from_slice(b"...");
        for byte in &mut region[3..] {
            if !matches!(byte, b'\n' | b'\r') {
                *byte = b' ';
            }
        }
    }
    let text = String::from_utf8(bytes).expect("blanking keeps the text valid UTF-8");
    let whole = TextRange::up_to(TextSize::of(source));
    let parsed = parse_module(&text).map_err(|error| located(source, &error, whole))?;
    Ok(parsed.into_syntax().body)
}

/// Parses `before + value + after` as a call expression, with the value at its original offset.
fn call(source: &str, range: TextRange, before: &str, after: &str) -> Result<ExprCall, Error> {
    let start = range.start().to_usize();
    let Some(padding) = start.checked_sub(before.len()) else {
        return Err(Error::new(source, "Expected a value", range));
    };
    let mut text = String::with_capacity(range.end().to_usize() + after.len());
    text.extend(std::iter::repeat_n(' ', padding));
    text.push_str(before);
    text.push_str(&source[range]);
    text.push_str(after);
    let whole = TextRange::new(TextSize::new(padding as u32), TextSize::of(text.as_str()));
    let parsed =
        parse_expression_range(&text, whole).map_err(|error| located(source, &error, range))?;
    match parsed.into_expr() {
        Expr::Call(call) => Ok(call),
        _ => Err(Error::new(source, "Expected a value", range)),
    }
}

/// Moves an error from the synthetic text into `range` of the Snakefile.
fn located(source: &str, error: &ParseError, range: TextRange) -> Error {
    let location = if range.contains_range(error.location) {
        error.location
    } else {
        TextRange::empty(error.location.start().clamp(range.start(), range.end()))
    };
    Error::new(source, error.error.to_string(), location)
}
