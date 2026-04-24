//! nf-validation 1.1.3, the plugin that nf-schema replaced: parameter validation, summaries
//! and help from a JSON schema of draft-07 with `definitions`, and `fromSamplesheet`. Older
//! nf-core pipelines use it, and Nextflow 26.04.6 still runs it with the legacy parser
//! ([decisions](../../../docs/decisions.md), item 14). It shares the JSON helpers of
//! [`crate::nf_schema`].
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod params;
mod samplesheet;

use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;

use crate::Context;
use crate::nf_schema::{Schema, entries, get, schema};

/// The functions of nf-validation that scripts can include; `fromSamplesheet` is a channel
/// factory, `Channel.fromSamplesheet(...)`.
pub(crate) const FUNCTIONS: &[&str] = &[
    "fromSamplesheet",
    "paramsHelp",
    "paramsSummaryLog",
    "paramsSummaryMap",
    "validateParameters",
];

pub(crate) fn call(
    name: &str,
    arguments: Vec<Value>,
    context: &mut Context,
) -> Result<Value, Error> {
    // Named arguments come first as a map of options.
    match (name, arguments.as_slice()) {
        ("validateParameters", []) => params::validate(None, context),
        ("validateParameters", [options]) => params::validate(Some(options), context),
        ("paramsSummaryMap", [workflow]) => params::summary(None, workflow, context),
        ("paramsSummaryMap", [options, workflow]) => {
            params::summary(Some(options), workflow, context)
        }
        ("paramsSummaryLog", [workflow]) => params::summary_log(None, workflow, context),
        ("paramsSummaryLog", [options, workflow]) => {
            params::summary_log(Some(options), workflow, context)
        }
        ("paramsHelp", [command]) => params::help(None, command, context),
        ("paramsHelp", [options, command]) => params::help(Some(options), command, context),
        ("fromSamplesheet", [param]) => samplesheet::from_samplesheet(None, param, context),
        ("fromSamplesheet", [options, param]) => {
            samplesheet::from_samplesheet(Some(options), param, context)
        }
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
