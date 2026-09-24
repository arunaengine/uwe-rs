//! A versioned description and typed validation response for schema-based input forms.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Value, json};
use workflow_graph::Driver;

use crate::application::{self, Admission, ApplicationError, Description, Policy};

/// The version of the form description and validation response.
pub const FORMAT: u32 = 1;

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

    /// JSON for a schema renderer. Values are effective launch values; defaults remain
    /// annotations. Callers control authorization and disclosure of schemas and values.
    /// `remote_content_checked` is always false: input-schema checks remote locators without
    /// fetching their bytes, checking object access, or binding immutable versions. Downloads
    /// during execution do not turn form validation into a remote-content check.
    pub fn json(&self) -> Value {
        let inputs = self.description.inputs.as_ref().expect("a form has inputs");
        json!({
            "version": FORMAT,
            "language": self.description.language.to_string(),
            "schema": inputs.schema,
            "values": inputs.values,
            "outputs": self.description.outputs,
            "validation": {
                "policy": match self.policy {
                    Policy::Relaxed => "relaxed",
                    Policy::Strict => "strict",
                },
                "values": "typed-json",
                "defaults": "annotations",
                "advisory": inputs.advisory,
                "ignored": inputs.ignored,
                "remote_content_checked": false,
            },
        })
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
        match self.check(values) {
            Ok(admission) => response(admission, None),
            Err(ApplicationError::InvalidInputs { admission, .. }) => {
                response(*admission, Some("input.invalid"))
            }
            Err(ApplicationError::AdvisorySchema { admission, .. }) => {
                response(*admission, Some("input.advisory_schema"))
            }
            Err(error) => json!({
                "version": FORMAT,
                "accepted": false,
                "error": "input.unavailable",
                "message": error.to_string(),
            }),
        }
    }
}

/// Describes a configured driver for a form without starting its workflow tasks.
pub fn describe(driver: &mut dyn Driver, policy: Policy) -> Result<Form, ApplicationError> {
    Form::new(application::describe(driver)?, policy)
}

fn response(admission: Admission, error: Option<&str>) -> Value {
    let mut response = admission
        .validation
        .expect("a form validates its input schema")
        .json();
    response["version"] = FORMAT.into();
    response["language"] = admission.language.to_string().into();
    response["accepted"] = error.is_none().into();
    response["error"] = error.map(Value::from).unwrap_or(Value::Null);
    response
}
