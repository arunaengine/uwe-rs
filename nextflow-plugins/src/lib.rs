//! Native versions of Nextflow plugin functions, which scripts include from `plugin/<id>`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod catalogue;
pub mod everit;
mod nf_anndata;
mod nf_arts;
mod nf_bactopia;
mod nf_bids;
mod nf_bigbrother;
mod nf_boost;
mod nf_cgroup_metrics;
mod nf_cloudcache;
mod nf_cmgg;
mod nf_co2footprint;
mod nf_core_utils;
mod nf_csvext;
mod nf_dotenv;
mod nf_drs;
mod nf_emoji;
mod nf_fairscape;
mod nf_fgbio;
mod nf_flowviewer;
mod nf_gc;
mod nf_irods;
mod nf_logsaver;
mod nf_metalog;
mod nf_parquet;
mod nf_ped;
mod nf_pregex;
mod nf_prometheus;
mod nf_prov;
mod nf_runinsights;
mod nf_schema;
mod nf_slack;
mod nf_snapshot;
mod nf_sqldb;
mod nf_storagemetrics;
mod nf_synapse;
mod nf_teams;
mod nf_tower;
mod nf_trino;
#[path = "validation.rs"]
mod nf_validation;
mod nf_versions;
mod nf_weblog;
pub mod observer;
mod report_format;
mod run_metadata;
mod service;

use std::any::Any;
use std::cell::{RefCell, RefMut};
use std::rc::Rc;

use nextflow_config::Config;
use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::value::{Native, Object, State as ObjectState, Value};

use crate::catalogue::{Kind, Plugin, State};

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

const NF_SCHEMA_PLUGIN: Plugin = Plugin {
    functions: NF_SCHEMA,
    call: Some(nf_schema::call),
    scopes: &["validation"],
    ..Plugin::new("nf-schema", Kind::Functions, State::Implemented)
};

const NF_VALIDATION_PLUGIN: Plugin = Plugin {
    functions: nf_validation::FUNCTIONS,
    call: Some(nf_validation::call),
    factories: &["fromSamplesheet"],
    scopes: &["validation"],
    ..Plugin::new("nf-validation", Kind::Functions, State::Implemented)
};

/// Whether nf-cgroup-metrics fills the memory fields of the trace from the cgroup samples.
pub fn cgroup_metrics(config: &Config) -> bool {
    nf_cgroup_metrics::enabled(config)
}

/// Where nf-cloudcache puts the cache store: see [`nf_cloudcache::location`].
pub fn cloud_cache(config: &Config, variable: Option<&str>) -> Result<Option<String>, String> {
    nf_cloudcache::location(config, variable)
}

/// Whether the plugins of the config free the intermediate files of the run while it runs:
/// nf-gc, or nf-boost with `boost.cleanup`. A config that they reject turns it off; their
/// observers report it.
pub fn cleanup(config: &Config) -> bool {
    let configured = catalogue::configured(config);
    let boost =
        configured.iter().any(|id| id == "nf-boost") && nf_boost::cleanup(config).unwrap_or(false);
    boost || nf_gc::mode(config).is_ok_and(|mode| mode.is_some())
}

/// The functions of a plugin that uwe runs, or `None` for a plugin uwe does not run.
pub fn functions(plugin: &str) -> Option<&'static [&'static str]> {
    let plugin = catalogue::find(plugin).filter(|plugin| plugin.works())?;
    Some(plugin.functions)
}

/// Native plugin coverage: every plugin of the catalogue, with the functions that work.
pub fn capabilities() -> serde_json::Value {
    catalogue::capabilities()
}

/// The plugin of a channel factory that a plugin adds, like `Channel.fromSamplesheet`.
pub fn factory_plugin(name: &str) -> Option<&'static str> {
    let plugins = catalogue::PLUGINS.iter();
    let mut adding = plugins.filter(|plugin| plugin.factories.contains(&name));
    adding.next().map(|plugin| plugin.id)
}

/// The script that ports functions of a plugin to Nextflow script, if there is one.
pub fn script(plugin: &str) -> Option<&'static str> {
    catalogue::find(plugin)?.script
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
    /// The `workflow` object of the run, which nf-schema 3 summaries read; null when unknown.
    pub workflow: Value,
}

/// Runs what the plugins of the config do when a run starts, before its script: nf-schema
/// checks its config, and may print help and end the run instead.
pub fn start(context: &mut Context) -> Result<(), Error> {
    if catalogue::configured(context.config)
        .iter()
        .any(|id| id == "nf-schema")
    {
        nf_schema::start(context)?;
    }
    if catalogue::configured(context.config)
        .iter()
        .any(|id| id == "nf-bactopia")
    {
        nf_bactopia::start(context)?;
    }
    Ok(())
}

/// Runs what the plugins of the config do when a run ends, after its `onComplete` handlers;
/// `context.workflow` is the finished `workflow` object.
pub fn finish(context: &mut Context) {
    if catalogue::configured(context.config)
        .iter()
        .any(|id| id == "nf-bactopia")
    {
        nf_bactopia::finish(context);
    }
}

/// Calls a method of a plugin object, `None` when the object has no such method.
pub fn method(
    object: &Value,
    name: &str,
    arguments: Vec<Value>,
    context: &mut Context,
) -> Result<Option<Value>, Error> {
    let plugin = match object {
        Value::Object(native) => match &native.borrow().state {
            ObjectState::Plugin(native) => native.plugin(),
            _ => return Ok(None),
        },
        _ => return Ok(None),
    };
    match catalogue::find(plugin).and_then(|plugin| plugin.methods) {
        Some(methods) => methods(object, name, arguments, context),
        None => Ok(None),
    }
}

/// A new object of a plugin class with its state.
pub(crate) fn object(class: &'static str, state: impl Native) -> Value {
    Value::Object(Rc::new(RefCell::new(Object {
        class,
        state: ObjectState::Plugin(Box::new(state)),
    })))
}

/// The state of a plugin object, if the value is an object of that state.
pub(crate) fn native<T: Native>(value: &Value) -> Option<RefMut<'_, T>> {
    let Value::Object(object) = value else {
        return None;
    };
    let borrowed = object.try_borrow_mut().ok()?;
    RefMut::filter_map(borrowed, |object| match &mut object.state {
        ObjectState::Plugin(native) => (native.as_mut() as &mut dyn Any).downcast_mut::<T>(),
        _ => None,
    })
    .ok()
}

/// Calls a function of a plugin.
pub fn call(
    plugin: &str,
    name: &str,
    arguments: Vec<Value>,
    context: &mut Context,
) -> Result<Value, Error> {
    match catalogue::find(plugin).and_then(|plugin| plugin.call) {
        Some(call) => call(name, arguments, context),
        None => Err(Error::Unsupported {
            message: format!("the function `{name}` of the plugin `{plugin}`"),
            span: Default::default(),
        }),
    }
}
