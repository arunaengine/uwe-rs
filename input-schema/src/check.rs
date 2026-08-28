//! Checks the values of a run against the schema of its inputs before any task starts, with the
//! same messages in every language: a required input without a value, a value of the wrong
//! type, a value outside an enum, and a file or directory that does not exist or is of the other
//! kind. Files at remote addresses are not looked at yet.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;

use jsonschema::ValidationError;
use jsonschema::error::{TypeKind, ValidationErrorKind as Kind};
use serde_json::Value as Json;
use workflow_graph::Inputs;

use crate::paths::validator;
use crate::values::prepared;

/// How bad a problem is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// The run cannot start.
    Error,
    /// The run starts, and the user should know.
    Warning,
}

/// A problem of the values of a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub severity: Severity,
    /// The input, like `input` or `samples[0].fastq_1`; empty for the inputs as a whole.
    pub input: String,
    /// What is wrong, in one sentence that names the input.
    pub message: String,
}

impl fmt::Display for Problem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

/// Checks the values of a run against the schema of its inputs. A simulated run assumes its
/// input files, so a file that does not exist is only a warning there, and so is a required
/// input without a value when the run makes up such values ([`Inputs::assumes_values`]). Every
/// problem of an advisory schema ([`Inputs::advisory`]) is a warning, and the values of ignored
/// inputs ([`Inputs::ignored`]) have none. A schema that is not valid gives one error.
pub fn check(inputs: &Inputs, simulated: bool) -> Vec<Problem> {
    let values = prepared(&inputs.schema, &inputs.schema, inputs.values.clone());
    let validator = match validator(&inputs.schema, &inputs.base) {
        Ok(validator) => validator,
        Err(message) => {
            return vec![Problem {
                severity: Severity::Error,
                input: String::new(),
                message: format!("the schema of the inputs is not valid: {message}"),
            }];
        }
    };
    let mut problems: Vec<Problem> = validator
        .iter_errors(&values)
        .filter(|error| !of_ignored(error, inputs))
        .map(|error| problem(&error, simulated, inputs.assumes_values))
        .collect();
    if inputs.advisory {
        for problem in &mut problems {
            problem.severity = Severity::Warning;
        }
    }
    problems
}

/// Whether an error concerns the value of an ignored input. A required input without a value
/// is an error of the object that lacks it, so ignoring an input does not make it optional.
fn of_ignored(error: &ValidationError<'_>, inputs: &Inputs) -> bool {
    let top = error.instance_path().as_str().split('/').nth(1);
    top.is_some_and(|name| {
        inputs
            .ignored
            .contains(&name.replace("~1", "/").replace("~0", "~"))
    })
}

/// The problem that a validation error stands for, in the words of iunctus.
fn problem(error: &ValidationError<'_>, simulated: bool, assumes_values: bool) -> Problem {
    let at = input_name(error.instance_path().as_str());
    let schema_path = error.schema_path().as_str();
    let keyword = schema_path.rsplit('/').next().unwrap_or_default();
    let mut severity = Severity::Error;
    let (input, message) = match error.kind() {
        Kind::Required { property } => {
            let input = join(&at, property.as_str().unwrap_or_default());
            let mut message = format!("{} is required, but has no value", subject(&input));
            if simulated && assumes_values {
                severity = Severity::Warning;
                message.push_str("; the simulated run makes one up");
            }
            (input, message)
        }
        Kind::Type { kind } => {
            let expected: Vec<String> = match kind {
                TypeKind::Single(kind) => vec![kind.to_string()],
                TypeKind::Multiple(kinds) => kinds.iter().map(|kind| kind.to_string()).collect(),
            };
            let expected: Vec<&str> = expected.iter().map(|kind| type_words(kind)).collect();
            let message = format!(
                "{} must be {}, but it is {}",
                subject(&at),
                alternatives(&expected),
                shown(error.instance())
            );
            (at, message)
        }
        Kind::Enum { options } => {
            let options: Vec<String> = match options {
                Json::Array(options) => options.iter().map(plain).collect(),
                other => vec![plain(other)],
            };
            let options: Vec<&str> = options.iter().map(String::as_str).collect();
            let message = format!(
                "{} must be one of {}, but it is {}",
                subject(&at),
                alternatives(&options),
                plain(error.instance())
            );
            (at, message)
        }
        Kind::Custom { message, .. } if keyword == "exists" => {
            let mut message = format!("{} {message}", subject(&at));
            if simulated {
                severity = Severity::Warning;
                message.push_str("; the simulated run assumes it");
            }
            (at, message)
        }
        Kind::Custom { message, .. } => {
            let message = format!("{} {message}", subject(&at));
            (at, message)
        }
        Kind::AnyOf { context } | Kind::OneOfNotValid { context } => {
            let choices: Vec<String> = context
                .iter()
                .map(|branch| {
                    let problems: Vec<Problem> = branch
                        .iter()
                        .map(|error| problem(error, simulated, assumes_values))
                        .collect();
                    if !problems.is_empty()
                        && problems
                            .iter()
                            .all(|problem| problem.severity == Severity::Warning)
                    {
                        severity = Severity::Warning;
                    }
                    let messages: Vec<String> = problems
                        .into_iter()
                        .map(|problem| problem.message)
                        .collect();
                    messages.join(" and ")
                })
                .filter(|choice| !choice.is_empty())
                .collect();
            let fit = if at.is_empty() { "fit" } else { "fits" };
            let message = format!(
                "{} {fit} none of the choices of the schema: {}",
                subject(&at),
                choices.join("; or ")
            );
            (at, message)
        }
        Kind::Not { schema } => {
            let names: Vec<String> = match schema.get("required") {
                Some(Json::Array(names)) => names
                    .iter()
                    .filter_map(Json::as_str)
                    .map(|name| format!("`{}`", join(&at, name)))
                    .collect(),
                _ => Vec::new(),
            };
            let message = match names.as_slice() {
                [] => format!("{} must not fit the schema `{schema}`", subject(&at)),
                [one] => format!("the input {one} must not be given"),
                many => format!("the inputs {} must not all be given", many.join(", ")),
            };
            (at, message)
        }
        _ => {
            let be = if at.is_empty() { "are" } else { "is" };
            let text: String = error.to_string().chars().take(200).collect();
            let message = format!("{} {be} not valid: {text}", subject(&at));
            (at, message)
        }
    };
    Problem {
        severity,
        input,
        message,
    }
}

/// `the input `x``, or `the inputs` for the whole.
fn subject(input: &str) -> String {
    match input {
        "" => "the inputs".into(),
        input => format!("the input `{input}`"),
    }
}

/// A JSON pointer like `/samples/0/fastq_1` as `samples[0].fastq_1`.
fn input_name(pointer: &str) -> String {
    let mut name = String::new();
    for part in pointer.split('/').skip(1) {
        let part = part.replace("~1", "/").replace("~0", "~");
        if part.parse::<usize>().is_ok() && !name.is_empty() {
            name.push_str(&format!("[{part}]"));
        } else {
            name = join(&name, &part);
        }
    }
    name
}

fn join(parent: &str, name: &str) -> String {
    match parent {
        "" => name.to_owned(),
        parent => format!("{parent}.{name}"),
    }
}

/// A JSON type in words.
fn type_words(kind: &str) -> &'static str {
    match kind {
        "string" => "a text",
        "integer" => "an integer",
        "number" => "a number",
        "boolean" => "true or false",
        "array" => "a list",
        "object" => "an object",
        "null" => "empty",
        _ => "of another type",
    }
}

/// `a`, `a or b`, `a, b or c`.
fn alternatives(words: &[&str]) -> String {
    match words {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

/// A value as the messages show it.
fn shown(value: &Json) -> String {
    match value {
        Json::String(text) => format!("the text `{text}`"),
        Json::Number(number) => format!("the number {number}"),
        Json::Bool(value) => format!("`{value}`"),
        Json::Null => "empty".into(),
        Json::Array(_) => "a list".into(),
        Json::Object(_) => "an object".into(),
    }
}

/// A value in backticks, texts without quotes.
fn plain(value: &Json) -> String {
    match value {
        Json::String(text) => format!("`{text}`"),
        other => format!("`{other}`"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_of_nested_inputs() {
        assert_eq!(input_name("/samples/0/fastq_1"), "samples[0].fastq_1");
        assert_eq!(input_name("/a~1b"), "a/b");
        assert_eq!(input_name(""), "");
    }
}
