//! A versioned description and typed validation response for schema-based input forms.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde::{Deserialize, Serialize};
use serde_json::Value;
use workflow_graph::Driver;

use crate::application::{self, Admission, ApplicationError, Description, Policy};

/// The version of the form description and validation response.
pub const FORMAT: u32 = 1;

/// The version 1 description returned to a form renderer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormResponse {
    pub language: String,
    pub outputs: Option<Value>,
    pub schema: Value,
    pub validation: ValidationSettings,
    pub values: Value,
    pub version: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationSettings {
    pub advisory: bool,
    pub defaults: String,
    pub ignored: std::collections::BTreeSet<String>,
    pub policy: String,
    pub remote_content_checked: bool,
    pub values: String,
}

/// The existing checked and unavailable response shapes, without extra wire fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ValidationResponse {
    Checked(CheckedResponse),
    Unavailable(UnavailableResponse),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckedResponse {
    pub accepted: bool,
    pub diagnostics: Vec<Diagnostic>,
    pub error: Option<String>,
    pub language: String,
    pub values: Value,
    pub version: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnavailableResponse {
    pub accepted: bool,
    pub error: String,
    pub message: String,
    pub version: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub branches: Vec<Vec<Diagnostic>>,
    pub code: String,
    pub input: String,
    pub instance_path: String,
    pub keyword: String,
    pub message: String,
    pub schema_path: String,
    pub severity: Severity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

impl From<input_schema::Diagnostic> for Diagnostic {
    fn from(value: input_schema::Diagnostic) -> Self {
        Self {
            branches: value
                .branches
                .into_iter()
                .map(|branch| branch.into_iter().map(Self::from).collect())
                .collect(),
            code: value.code,
            input: value.input,
            instance_path: value.instance_path,
            keyword: value.keyword,
            message: value.message,
            schema_path: value.schema_path,
            severity: match value.severity {
                input_schema::Severity::Error => Severity::Error,
                input_schema::Severity::Warning => Severity::Warning,
            },
        }
    }
}

/// A form backed by the server's configured workflow description.
/// The original schema retains references, compositions, defaults and file annotations.
#[derive(Clone, Debug)]
pub struct Form {
    description: Description,
    policy: Policy,
}

impl Form {
    /// Creates a form from a description obtained with `application::describe`.
    pub fn new(description: Description, policy: Policy) -> Result<Self, ApplicationError> {
        if description.inputs.is_none() {
            return Err(ApplicationError::MissingSchema {
                language: description.language,
            });
        }
        Ok(Self {
            description,
            policy,
        })
    }

    /// JSON with effective values and defaults as annotations, for authorized renderers.
    /// Remote bytes, access and immutable versions remain unchecked during validation.
    /// See docs/design/embedding.md for disclosure and remote-content boundaries.
    pub fn json(&self) -> Value {
        serde_json::to_value(self.response()).expect("form responses serialize")
    }

    /// Typed description with the same version 1 JSON shape as `json`.
    pub fn response(&self) -> FormResponse {
        let inputs = self.description.inputs.as_ref().expect("a form has inputs");
        FormResponse {
            version: FORMAT,
            language: self.description.language.to_string(),
            schema: inputs.schema.clone(),
            values: inputs.values.clone(),
            outputs: self.description.outputs.clone(),
            validation: ValidationSettings {
                policy: match self.policy {
                    Policy::Relaxed => "relaxed".into(),
                    Policy::Strict => "strict".into(),
                },
                values: "typed-json".into(),
                defaults: "annotations".into(),
                advisory: inputs.advisory,
                ignored: inputs.ignored.clone(),
                remote_content_checked: false,
            },
        }
    }

    /// Checks a posted JSON object against this server-held schema without starting tasks
    /// or changing a driver. Configure and admit the submission's driver before execution.
    pub fn check(&self, values: Value) -> Result<Admission, ApplicationError> {
        let mut description = self.description.clone();
        description
            .inputs
            .as_mut()
            .expect("a form has inputs")
            .values = values;
        application::validate_json(description, false, self.policy)
    }

    /// JSON for a form's validation action, including exact field pointers and branches.
    /// The request supplies values only; the server retains the schema and policy.
    pub fn validate(&self, values: Value) -> Value {
        serde_json::to_value(self.validate_response(values)).expect("form responses serialize")
    }

    /// Typed validation response with unchanged version 1 fields and diagnostics.
    pub fn validate_response(&self, values: Value) -> ValidationResponse {
        match self.check(values) {
            Ok(admission) => response(admission, None),
            Err(ApplicationError::InvalidInputs { admission, .. }) => {
                response(*admission, Some("input.invalid"))
            }
            Err(ApplicationError::AdvisorySchema { admission, .. }) => {
                response(*admission, Some("input.advisory_schema"))
            }
            Err(error) => ValidationResponse::Unavailable(UnavailableResponse {
                version: FORMAT,
                accepted: false,
                error: "input.unavailable".into(),
                message: error.to_string(),
            }),
        }
    }
}

/// Describes a configured driver for a form without starting its workflow tasks.
pub fn describe(driver: &mut dyn Driver, policy: Policy) -> Result<Form, ApplicationError> {
    Form::new(application::describe(driver)?, policy)
}

fn response(admission: Admission, error: Option<&str>) -> ValidationResponse {
    let validation = admission
        .validation
        .expect("a form validates its input schema");
    ValidationResponse::Checked(CheckedResponse {
        accepted: error.is_none(),
        diagnostics: validation
            .diagnostics
            .into_iter()
            .map(Diagnostic::from)
            .collect(),
        error: error.map(str::to_owned),
        language: admission.language.to_string(),
        values: validation.values,
        version: FORMAT,
    })
}
