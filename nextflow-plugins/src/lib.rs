//! Native versions of Nextflow plugin functions, which scripts include from `plugin/<id>`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod everit;
mod nf_anndata;
mod nf_fgbio;
mod nf_schema;
#[path = "validation.rs"]
mod nf_validation;

use nextflow_config::Config;
use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::value::Value;

/// Parameters as nf-schema validates them, according to the selected plugin version.
pub fn schema_params(params: &Value, config: &Config) -> Value {
    let old = nf_schema::schema_version(config)
        .is_some_and(|parts| parts.as_slice() < [2, 7, 0].as_slice());
    clean_params(params, old)
}

/// Removes absent parameter values using the selected nf-schema cleaning rules.
pub fn clean_params(params: &Value, old: bool) -> Value {
    clean_value(params, old).unwrap_or_else(|| Value::map([]))
}

pub(crate) fn clean_value(value: &Value, old: bool) -> Option<Value> {
    let zero = matches!(value, Value::Number(number) if number.is_zero());
    if matches!(value, Value::Null) || old && !value.truth() && !zero {
        return None;
    }
    Some(match value {
        Value::Map(_) => Value::map(
            nf_schema::entries(value)
                .into_iter()
                .filter_map(|(key, value)| Some((Value::string(key), clean_value(&value, old)?))),
        ),
        other => other.clone(),
    })
}

/// The functions of nf-schema that scripts can include.
const NF_SCHEMA: &[&str] = &[
    "paramsHelp",
    "paramsSummaryLog",
    "paramsSummaryMap",
    "samplesheetToList",
    "validate",
    "validateParameters",
];

/// The functions of nf-core-utils 0.5.0. Those that `nf_core_utils.nf` defines run as
/// script code; the others are not supported yet.
const NF_CORE_UTILS: &[&str] = &[
    "autoToolBibliographyText",
    "autoToolCitationText",
    "checkCondaChannels",
    "checkConfigProvided",
    "checkProfileProvided",
    "collectCitationsFromFiles",
    "collectVersions",
    "completionEmail",
    "completionSummary",
    "convertLegacyYamlToEvalSyntax",
    "convertMetaYamlToTopicFormat",
    "dumpParametersToJSON",
    "generateCitationReport",
    "generateComprehensiveReport",
    "generateModuleToolCitation",
    "generateVersionReport",
    "generateYamlFromEvalSyntax",
    "getCitation",
    "getGenomeAttribute",
    "getReferencesFile",
    "getReferencesValue",
    "getWorkflowVersion",
    "imNotification",
    "methodsDescriptionText",
    "paramsSummaryMultiqc",
    "processCitationsFromFile",
    "processCitationsFromTopic",
    "processMixedCitationSources",
    "processMixedVersionSources",
    "processVersionsFromFile",
    "processVersionsFromTopic",
    "softwareVersionsToYAML",
    "toolBibliographyText",
    "toolCitationText",
    "workflowVersionToChannel",
];

/// The functions of a plugin, or `None` for a plugin uwe does not have.
pub fn functions(plugin: &str) -> Option<&'static [&'static str]> {
    match plugin {
        "nf-anndata" => Some(&["anndata"]),
        "nf-core-utils" => Some(NF_CORE_UTILS),
        "nf-fgbio" => Some(&["readStructure"]),
        "nf-schema" => Some(NF_SCHEMA),
        "nf-validation" => Some(nf_validation::FUNCTIONS),
        _ => None,
    }
}

/// Native plugin coverage, including functions recognized but not implemented.
pub fn capabilities() -> serde_json::Value {
    let core = [
        "getGenomeAttribute",
        "getWorkflowVersion",
        "checkConfigProvided",
        "checkProfileProvided",
        "completionSummary",
        "dumpParametersToJSON",
        "softwareVersionsToYAML",
    ];
    let unavailable: Vec<_> = NF_CORE_UTILS
        .iter()
        .filter(|name| !core.contains(name))
        .collect();
    serde_json::json!({
        "nf-schema": {"supported": NF_SCHEMA, "unavailable": []},
        "nf-validation": {"supported": nf_validation::FUNCTIONS, "unavailable": []},
        "nf-fgbio": {"supported": ["readStructure"], "unavailable": []},
        "nf-core-utils": {"supported": core, "unavailable": unavailable},
        "nf-anndata": {"supported": [], "unavailable": ["anndata"]}
    })
}

/// The plugin of a channel factory that a plugin adds, like `Channel.fromSamplesheet`.
pub fn factory_plugin(name: &str) -> Option<&'static str> {
    match name {
        "fromSamplesheet" => Some("nf-validation"),
        _ => None,
    }
}

/// The script that ports functions of a plugin to Nextflow script, if there is one.
pub fn script(plugin: &str) -> Option<&'static str> {
    match plugin {
        "nf-core-utils" => Some(include_str!("nf_core_utils.nf")),
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
    /// The names of the params of the command line and the params file, which Nextflow keeps
    /// as `session.cliParams`.
    pub cli_params: &'a [String],
}

/// Calls a function of a plugin.
pub fn call(
    plugin: &str,
    name: &str,
    arguments: Vec<Value>,
    context: &mut Context,
) -> Result<Value, Error> {
    match plugin {
        "nf-anndata" => nf_anndata::call(name, arguments, context),
        "nf-fgbio" => nf_fgbio::call(name, arguments),
        "nf-schema" => nf_schema::call(name, arguments, context),
        "nf-validation" => nf_validation::call(name, arguments, context),
        _ => Err(Error::Unsupported {
            message: format!("the function `{name}` of the plugin `{plugin}`"),
            span: Default::default(),
        }),
    }
}
