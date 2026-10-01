//! Checks workflow input values and local paths against JSON Schema before execution.
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

/// One machine-readable problem, with its location in the values and schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub input: String,
    pub message: String,
    /// RFC 6901 JSON Pointer into the normalized values.
    pub instance_path: String,
    /// RFC 6901 JSON Pointer into the schema.
    pub schema_path: String,
    /// The JSON Schema keyword, such as `required`, `type` or `anyOf`.
    pub keyword: String,
    /// A stable code for programmatic consumers, such as `schema.required`.
    pub code: String,
    /// The errors of each branch of `anyOf` or `oneOf`, in schema order.
    pub branches: Vec<Vec<Diagnostic>>,
}

/// The normalized values and all validation diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Validation {
    pub values: Json,
    pub diagnostics: Vec<Diagnostic>,
}

impl Validation {
    /// This result as JSON for forms and other machine consumers.
    pub fn json(&self) -> Json {
        serde_json::json!({
            "values": self.values,
            "diagnostics": self.diagnostics.iter().map(diagnostic_json).collect::<Vec<_>>()
        })
    }
}

/// Checks input values; invalid schemas give one error and ignored inputs give no problems.
/// Advisory failures, simulated missing files, and assumed missing values are warnings.
/// See the crate README for validation details.
pub fn check(inputs: &Inputs, simulated: bool) -> Vec<Problem> {
    validate(inputs, simulated)
        .diagnostics
        .into_iter()
        .map(|diagnostic| Problem {
            severity: diagnostic.severity,
            input: diagnostic.input,
            message: diagnostic.message,
        })
        .collect()
}

/// Checks and returns normalized values with machine-readable diagnostics. Defaults remain
/// schema annotations and are not inserted into the values.
pub fn validate(inputs: &Inputs, simulated: bool) -> Validation {
    let values = prepared(&inputs.schema, &inputs.schema, inputs.values.clone());
    validate_values(inputs, values, simulated)
}

/// Checks already typed JSON without applying command-line string coercion. The submitted
/// values are preserved exactly, and defaults remain schema annotations.
pub fn validate_json(inputs: &Inputs, simulated: bool) -> Validation {
    validate_values(inputs, inputs.values.clone(), simulated)
}

fn validate_values(inputs: &Inputs, values: Json, simulated: bool) -> Validation {
    let validator = match validator(&inputs.schema, &inputs.base) {
        Ok(validator) => validator,
        Err(message) => {
            return Validation {
                values,
                diagnostics: vec![Diagnostic {
                    severity: Severity::Error,
                    input: String::new(),
                    message: format!("the schema of the inputs is not valid: {message}"),
                    instance_path: String::new(),
                    schema_path: String::new(),
                    keyword: "schema".into(),
                    code: "schema.invalid".into(),
                    branches: Vec::new(),
                }],
            };
        }
    };
    let mut diagnostics: Vec<Diagnostic> = validator
        .iter_errors(&values)
        .filter(|error| !of_ignored(error, inputs))
        .map(|error| diagnostic(&error, simulated, inputs.assumes_values))
        .collect();
    if inputs.advisory {
        for diagnostic in &mut diagnostics {
            warning(diagnostic);
        }
    }
    Validation {
        values,
        diagnostics,
    }
}

fn diagnostic(error: &ValidationError<'_>, simulated: bool, assumes_values: bool) -> Diagnostic {
    let problem = problem(error, simulated, assumes_values);
    let keyword = error.kind().keyword().to_owned();
    let mut instance_path = error.instance_path().to_string();
    if let Kind::Required { property } = error.kind()
        && let Some(property) = property.as_str()
    {
        instance_path = pointer_join(&instance_path, property);
    }
    let branches = match error.kind() {
        Kind::AnyOf { context }
        | Kind::OneOfNotValid { context }
        | Kind::OneOfMultipleValid { context } => context
            .iter()
            .map(|branch| {
                branch
                    .iter()
                    .map(|error| diagnostic(error, simulated, assumes_values))
                    .collect()
            })
            .collect(),
        _ => Vec::new(),
    };
    Diagnostic {
        severity: problem.severity,
        input: problem.input,
        message: problem.message,
        instance_path,
        schema_path: error.schema_path().to_string(),
        code: format!("schema.{keyword}"),
        keyword,
        branches,
    }
}

fn warning(diagnostic: &mut Diagnostic) {
    diagnostic.severity = Severity::Warning;
    for branch in &mut diagnostic.branches {
        for diagnostic in branch {
            warning(diagnostic);
        }
    }
}

fn diagnostic_json(diagnostic: &Diagnostic) -> Json {
    serde_json::json!({
        "severity": match diagnostic.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        },
        "input": diagnostic.input,
        "message": diagnostic.message,
        "instance_path": diagnostic.instance_path,
        "schema_path": diagnostic.schema_path,
        "keyword": diagnostic.keyword,
        "code": diagnostic.code,
        "branches": diagnostic.branches.iter().map(|branch| {
            branch.iter().map(diagnostic_json).collect::<Vec<_>>()
        }).collect::<Vec<_>>()
    })
}

fn pointer_join(parent: &str, property: &str) -> String {
    let property = property.replace('~', "~0").replace('/', "~1");
    format!("{parent}/{property}")
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
    fn reports_nested_names() {
        assert_eq!(input_name("/samples/0/fastq_1"), "samples[0].fastq_1");
        assert_eq!(input_name("/a~1b"), "a/b");
        assert_eq!(input_name(""), "");
    }
}
