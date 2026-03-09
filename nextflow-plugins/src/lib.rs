//! Native versions of Nextflow plugin functions, which scripts include from `plugin/<id>`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_config::Config;
use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::value::Value;

/// The functions of nf-schema that scripts can include.
const NF_SCHEMA: &[&str] = &[
    "paramsHelp",
    "paramsSummaryLog",
    "paramsSummaryMap",
    "samplesheetToList",
    "validateParameters",
];

/// The functions of a plugin, or `None` for a plugin iunctus does not have.
pub fn functions(plugin: &str) -> Option<&'static [&'static str]> {
    match plugin {
        "nf-schema" => Some(NF_SCHEMA),
        _ => None,
    }
}

/// What plugin functions can see of the run.
pub struct Context<'a> {
    /// Reads files, prints and knows the launch directory.
    pub host: &'a mut dyn Host,
    pub config: &'a Config,
    /// The params of the run, with those that scripts set.
    pub params: Value,
    /// The directory of the main script.
    pub project_dir: &'a str,
}

/// Calls a function of a plugin.
pub fn call(
    plugin: &str,
    name: &str,
    _arguments: Vec<Value>,
    _context: &mut Context,
) -> Result<Value, Error> {
    Err(Error::Unsupported {
        message: format!("the function `{name}` of the plugin `{plugin}`"),
        span: Default::default(),
    })
}
