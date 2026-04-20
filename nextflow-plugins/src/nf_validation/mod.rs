//! nf-validation 1.1.3, the plugin that nf-schema replaced: parameter validation, summaries
//! and help from a JSON schema of draft-07 with `definitions`, and `fromSamplesheet`. Older
//! nf-core pipelines use it, and Nextflow 26.04.6 still runs it with the legacy parser
//! ([decisions](../../../docs/decisions.md), item 14). It shares the JSON helpers of
//! [`crate::nf_schema`].
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod params;

use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;

use crate::Context;
use crate::nf_schema::{Schema, entries, get, json_type, plain, schema};

/// The functions of nf-validation that scripts can include; `fromSamplesheet` is a channel
/// factory, `Channel.fromSamplesheet(...)`.
pub(crate) const FUNCTIONS: &[&str] = &["validateParameters"];

pub(crate) fn call(
    name: &str,
    arguments: Vec<Value>,
    context: &mut Context,
) -> Result<Value, Error> {
    // Named arguments come first as a map of options.
    match (name, arguments.as_slice()) {
        ("validateParameters", []) => params::validate(None, context),
        ("validateParameters", [options]) => params::validate(Some(options), context),
        _ => Err(Error::Unsupported {
            message: format!(
                "the function `{name}` of the plugin `nf-validation` with these arguments"
            ),
            span: Default::default(),
        }),
    }
}

/// The parameters schema of the `parameters_schema` option, else `nextflow_schema.json`.
fn parameters_schema(options: Option<&Value>, context: &mut Context) -> Result<Schema, Error> {
    let path = options
        .and_then(|options| get(options, "parameters_schema"))
        .filter(Value::truth)
        .unwrap_or_else(|| Value::string("nextflow_schema.json"));
    schema(&path, context)
}

/// The params by group title: each group of `definitions`, then the params outside any
/// group as `Other parameters`, as nf-validation's `paramsRead` gives them.
fn groups(schema: &Value) -> Vec<(String, Vec<(String, Value)>)> {
    let mut groups = Vec::new();
    for (name, group) in get(schema, "definitions").map_or_else(Vec::new, |d| entries(&d)) {
        let title = get(&group, "title").map_or(name, |title| title.to_string());
        let properties = get(&group, "properties").map_or_else(Vec::new, |p| entries(&p));
        groups.push((title, properties));
    }
    if let Some(properties) = get(schema, "properties") {
        groups.push(("Other parameters".into(), entries(&properties)));
    }
    groups
}

/// Whether a param of nf-validation is set, like `params.validationLenientMode`.
fn flag(context: &Context, name: &str) -> bool {
    get(&context.params, name).is_some_and(|value| value.truth())
}

/// One problem of a validation, in the words of the everit validator that nf-validation
/// uses.
enum Problem {
    /// A required property is missing.
    Missing { name: String },
    /// The value at `path` is wrong.
    Value {
        path: String,
        message: String,
        /// For an enum, the choices, which nf-validation lists itself for params.
        choices: Option<Vec<String>>,
    },
}

/// The problems of an instance, as everit words them.
fn problems(validator: &jsonschema::Validator, instance: &serde_json::Value) -> Vec<Problem> {
    use jsonschema::error::{TypeKind, ValidationErrorKind as Kind};
    let mut problems = Vec::new();
    for error in validator.iter_errors(instance) {
        let path = error.instance_path().to_string();
        let value = error.instance();
        let (message, choices) = match error.kind() {
            Kind::Required { property } => {
                let name = property.as_str().unwrap_or_default().to_owned();
                problems.push(Problem::Missing { name });
                continue;
            }
            Kind::Type { kind } => {
                let expected = match kind {
                    TypeKind::Single(kind) => java_class(&kind.to_string()).to_owned(),
                    TypeKind::Multiple(kinds) => {
                        let kinds: Vec<&str> = kinds
                            .iter()
                            .map(|kind| java_class(&kind.to_string()))
                            .collect();
                        kinds.join(", ")
                    }
                };
                let found = java_class(json_type(value));
                (format!("expected type: {expected}, found: {found}"), None)
            }
            Kind::Pattern { pattern } => (
                format!("string [{}] does not match pattern {pattern}", plain(value)),
                None,
            ),
            Kind::Enum { options } => {
                let choices = options
                    .as_array()
                    .map(|items| items.iter().map(plain).collect());
                (
                    format!("{} is not a valid enum value", plain(value)),
                    choices,
                )
            }
            _ => (error.to_string(), None),
        };
        problems.push(Problem::Value {
            path,
            message,
            choices,
        });
    }
    problems
}

/// The Java class that everit names for a JSON type.
fn java_class(kind: &str) -> &'static str {
    match kind {
        "string" => "String",
        "integer" => "Integer",
        "number" => "Number",
        "boolean" => "Boolean",
        "object" => "JSONObject",
        "array" => "JSONArray",
        _ => "Null",
    }
}
