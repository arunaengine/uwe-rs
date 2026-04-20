//! Native versions of Nextflow plugin functions, which scripts include from `plugin/<id>`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod nf_anndata;
mod nf_fgbio;
mod nf_schema;
mod nf_validation;

use nextflow_config::Config;
use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::value::Value;

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

/// The functions of a plugin, or `None` for a plugin iunctus does not have.
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
