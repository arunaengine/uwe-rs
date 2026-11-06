//! nf-python 0.1.4: `pyFunction` runs Python code in a container, one fresh process per call,
//! and gives back the values that the code hands to `nf.output`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub(crate) mod container;
mod exchange;

use std::path::{Path, PathBuf};

use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;
use sha2::{Digest, Sha256};

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};
use crate::nf_schema::entries;
use crate::nf_sqldb::{split_options, thrown};
use container::{Call, Ended};

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["pyFunction"],
    call: Some(call),
    scopes: &["nf_python"],
    ..Plugin::new("nf-python", Kind::Functions, State::Partial)
};

/// The image that runs the calls when `nf_python.container` names none: the Python image
/// that uwe's Snakemake support uses too.
pub(crate) const IMAGE: &str = "ghcr.io/astral-sh/uv:0.9.30-python3.12-bookworm-slim";

/// uwe's `nf_python` module, which the calls import.
const MODULE: &str = include_str!("module.py");

/// The named arguments that set up the call rather than go to Python.
const RESERVED: [&str; 3] = ["script", "_executable", "_conda_env"];

fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    match name {
        "pyFunction" => py_function(arguments, context),
        _ => Err(Error::Unsupported {
            message: format!("the function `{name}` of the plugin `nf-python`"),
            span: Default::default(),
        }),
    }
}

/// A text setting, which must be a string.
fn setting(value: Option<Value>) -> Result<Option<String>, Error> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text) | Value::GString(text)) => Ok(Some(text.to_string())),
        Some(other) => Err(thrown(format!("Expected a string, got: {other}"))),
    }
}

/// The Python program, from the call or else the config; uwe runs it inside the container.
fn program(options: &[(String, Value)], context: &Context) -> Result<String, Error> {
    let option = |name: &str| crate::nf_sqldb::option(options, name);
    let (executable, conda) = match option("_executable").or(option("_conda_env")) {
        Some(_) => (option("_executable"), option("_conda_env")),
        None => (
            context.config.get(&["nf_python", "executable"]),
            context.config.get(&["nf_python", "conda_env"]),
        ),
    };
    let (executable, conda) = (setting(executable)?, setting(conda)?);
    if executable.is_some() && conda.is_some() {
        return Err(thrown(
            "The 'executable' and 'conda_env' options cannot be used together",
        ));
    }
    if let Some(conda) = conda {
        return Err(thrown(format!(
            "uwe runs nf-python in a container and cannot use the conda environment {conda}; \
             set nf_python.container to an image with its packages instead"
        )));
    }
    Ok(executable.unwrap_or("python".to_owned()))
}

/// `pyFunction(code, [name: value, ...])` or `pyFunction(script: 'file.py', ...)`.
fn py_function(arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    let (options, positional) = split_options(arguments);
    let code = (positional.into_iter())
        .find(|value| !matches!(value, Value::Null))
        .map(|value| value.to_string())
        .filter(|code| !code.is_empty());
    let script = crate::nf_sqldb::option(&options, "script").map(|value| value.to_string());
    let program = program(&options, context)?;
    let launch = PathBuf::from(context.host.launch_dir());
    let (source, file) = match (&code, &script) {
        (Some(_), Some(_)) => {
            return Err(thrown("Cannot use both code and script options together"));
        }
        (None, None) => return Err(thrown("Missing code argument")),
        (Some(code), None) => (code.clone(), None),
        (None, Some(script)) => {
            let file = launch.join(script);
            let text = std::fs::read_to_string(&file).map_err(|error| {
                thrown(format!(
                    "cannot read the script {}: {error}",
                    file.display()
                ))
            })?;
            (text, Some(file))
        }
    };
    let passed: Vec<(String, Value)> = (options.into_iter())
        .filter(|(name, _)| !RESERVED.contains(&name.as_str()))
