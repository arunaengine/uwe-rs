//! nf-core-utils 0.5.0 to 0.7.0: the helpers of the nf-core pipeline template.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod conda;
mod mail;
mod webhook;

use nextflow_config::Config;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};

/// `functions.nf` ports some functions, and calls the helpers of `call` for the config,
/// conda, mail and webhooks.
pub(crate) const PLUGIN: Plugin = Plugin {
    functions: FUNCTIONS,
    supported: Some(PORTED),
    call: Some(call),
    script: Some(include_str!("functions.nf")),
    scopes: &["mail"],
    ..Plugin::new("nf-core-utils", Kind::Functions, State::Partial)
};

/// The functions of nf-core-utils 0.5.0. Those that `functions.nf` defines run as
/// script code; the others are not supported yet.
const FUNCTIONS: &[&str] = &[
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

/// The functions of nf-core-utils that `functions.nf` defines.
const PORTED: &[&str] = &[
    "getGenomeAttribute",
    "getWorkflowVersion",
    "checkConfigProvided",
    "checkProfileProvided",
    "completionSummary",
    "dumpParametersToJSON",
    "softwareVersionsToYAML",
];

/// The release that a config without a pinned version gets, the latest.
const LATEST: &str = "0.7.0";

fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    match (name, arguments.as_slice()) {
        // The helpers that only `functions.nf` can include.
        ("coreSettings", []) => Ok(settings(context.config)),
        ("condaChannels", []) => Ok(conda::channels(&*context.host)),
        ("deliverMail", [message]) => Ok(mail::deliver(message, context)),
        ("postNotification", [url, body]) => Ok(webhook::post(&url.to_string(), &body.to_string())),
        _ => Err(Error::Unsupported {
            message: format!("the function `{name}` of the plugin `nf-core-utils`"),
            span: Default::default(),
        }),
    }
}

/// The pinned release, like `0.7.0`, and the `nextflow.version` setting of the config.
fn settings(config: &Config) -> Value {
    let version = release(config).unwrap_or_else(|| LATEST.to_owned());
    let nextflow = config.get(&["nextflow", "version"]).unwrap_or(Value::Null);
    Value::map([
        (Value::string("version"), Value::string(version)),
        (Value::string("nextflow"), nextflow),
    ])
}

/// The version of `id 'nf-core-utils@<version>'` in the `plugins` block.
fn release(config: &Config) -> Option<String> {
    let Some(Value::Set(plugins)) = config.get(&["plugins"]) else {
        return None;
    };
    let plugins = plugins.borrow();
    plugins.items.iter().find_map(|id| {
        let id = id.0.to_string();
        let version = id.trim().strip_prefix("nf-core-utils@")?.trim().to_owned();
        Some(version).filter(|version| !version.is_empty())
    })
}

/// A setting of the config as text, `None` when it is absent or null.
fn text_setting(config: &Config, path: &[&str]) -> Option<String> {
    match config.get(path) {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.to_string()),
    }
}
