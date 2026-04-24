//! JSON schema validation in the words of the everit validator (`org.everit.json.schema`),
//! which Nextflow releases before 24 shipped. nf-validation and the `lib/` classes of older
//! nf-core pipelines report its messages, and those classes read its `toJSON()`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Value as Json, json};

use crate::nf_schema::{json_type, plain};

/// One problem of a validation.
pub(crate) enum Problem {
    /// A required property is missing from the object at `path`.
    Missing { path: String, name: String },
    /// The value at `path` is wrong.
    Value {
        path: String,
        message: String,
        /// The schema keyword that fails, like `type`.
        keyword: String,
        /// For an enum, the choices, which nf-validation lists itself for params.
        choices: Option<Vec<String>>,
    },
}

/// The problems of an instance, as everit words them.
pub(crate) fn problems(validator: &jsonschema::Validator, instance: &Json) -> Vec<Problem> {
    use jsonschema::error::{TypeKind, ValidationErrorKind as Kind};
    let mut problems = Vec::new();
    for error in validator.iter_errors(instance) {
        let path = error.instance_path().to_string();
        let value = error.instance();
        let (message, choices) = match error.kind() {
            Kind::Required { property } => {
                let name = property.as_str().unwrap_or_default().to_owned();
                problems.push(Problem::Missing { path, name });
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
        let schema_path = error.schema_path().to_string();
        let keyword = schema_path
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_owned();
        problems.push(Problem::Value {
            path,
            message,
            keyword,
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

/// Validates the JSON text of an instance against the JSON text of a schema, as
/// `SchemaLoader.load(schema).validate(instance)` does. The result is `None` when the
/// instance is valid, else the violation as everit's `ValidationException.toJSON()` gives
/// it: one problem, or a violation of the whole instance that lists them.
pub fn violation(schema: &str, instance: &str) -> Result<Option<Json>, String> {
    let schema: Json = serde_json::from_str(schema).map_err(|error| error.to_string())?;
    let instance: Json = serde_json::from_str(instance).map_err(|error| error.to_string())?;
    let validator = jsonschema::validator_for(&schema).map_err(|error| error.to_string())?;
    let mut causes: Vec<Json> = problems(&validator, &instance)
        .into_iter()
        .map(|problem| match problem {
            Problem::Missing { path, name } => leaf(
                &path,
                &format!("required key [{name}] not found"),
                "required",
            ),
            Problem::Value {
                path,
                message,
                keyword,
                ..
            } => leaf(&path, &message, &keyword),
        })
        .collect();
    Ok(match causes.len() {
        0 => None,
        1 => causes.pop(),
        count => Some(json!({
            "schemaLocation": "#",
            "pointerToViolation": "#",
            "causingExceptions": causes,
            "keyword": Json::Null,
            "message": format!("{count} schema violations found"),
        })),
    })
}

/// A violation without causes, at the JSON pointer `path` of the instance.
fn leaf(path: &str, message: &str, keyword: &str) -> Json {
    json!({
        "schemaLocation": "#",
        "pointerToViolation": format!("#{path}"),
        "causingExceptions": [],
        "keyword": keyword,
        "message": message,
    })
}
