//! `paramsHelp`: the params of a parameters schema by group.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;

use super::get;
use super::params::{groups, parameters_schema};
use crate::Context;

/// `paramsHelp`: each group of the schema with its params and their descriptions.
pub(super) fn text(options: Option<&Value>, context: &mut Context) -> Result<Value, Error> {
    let schema = parameters_schema(options, context)?;
    let mut output = String::new();
    for (title, properties) in groups(&schema.tree) {
        output.push_str(&format!("{title}\n"));
        for (name, definition) in properties {
            let kind = get(&definition, "type").map_or(String::new(), |kind| kind.to_string());
            let description =
                get(&definition, "description").map_or(String::new(), |d| d.to_string());
            output.push_str(&format!("  --{name:<30} [{kind}] {description}\n"));
        }
        output.push('\n');
    }
    Ok(Value::string(output))
}
