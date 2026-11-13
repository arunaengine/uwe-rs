//! The settings of an nf-r-ipc call, from its `_` options and the `nfR` scope, and the runtime
//! that they choose after the plugin's checks: uwe runs every call in a container.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;

use crate::Context;
use crate::nf_sqldb::thrown;

/// The image of the calls when the config names none: R, which installs `nanoarrow` into the
/// work directory on first use.
const IMAGE: &str = "rocker/r-ver:4.5.1";

/// The settings that a call takes with a leading `_` and the config in the `nfR` scope.
const SETTINGS: [&str; 13] = [
    "runtime",
    "executable",
    "conda_env",
    "r_libs",
    "on_error",
    "payload_kind",
    "docker_image",
    "docker_container",
    "docker_executable",
    "docker_args",
    "docker_mounts",
    "docker_workdir",
    "container_rscript",
];

/// The settings of a call: its own `_` options win over the config; blank text is unset.
pub(super) struct Settings {
    values: Vec<(&'static str, Value)>,
}

impl Settings {
    pub(super) fn of(options: &[(String, Value)], context: &Context) -> Settings {
        let blank = |value: &Value| match value {
            Value::Null => true,
            Value::String(text) | Value::GString(text) => text.trim().is_empty(),
            _ => false,
        };
        let mut values = Vec::new();
        for name in SETTINGS {
            let own = options
                .iter()
                .find(|(key, _)| key.strip_prefix('_') == Some(name));
            let own = own
                .map(|(_, value)| value.clone())
                .filter(|value| !blank(value));
            let config = context
                .config
                .get(&["nfR", name])
                .filter(|value| !blank(value));
            if let Some(value) = own.or(config) {
                values.push((name, value));
            }
        }
        Settings { values }
    }

    pub(super) fn text(&self, name: &str) -> Option<String> {
        let found = self.values.iter().find(|(key, _)| *key == name);
        found.map(|(_, value)| value.to_string().trim().to_owned())
    }

    /// A list setting: a list, or text split on spaces.
    pub(super) fn list(&self, name: &str) -> Vec<String> {
        match self.values.iter().find(|(key, _)| *key == name) {
            Some((_, Value::List(list))) => {
                list.borrow().items.iter().map(Value::to_string).collect()
            }
            Some((_, other)) => other
                .to_string()
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
            None => Vec::new(),
        }
    }
}

/// Where a call runs in uwe: an image and the R program in it.
pub(super) struct Runtime {
    pub name: String,
    pub image: String,
    pub program: String,
    /// The `runtime` map of the answer.
    pub shown: Vec<(String, Value)>,
}

/// The runtime of a call after the plugin's checks; uwe runs every call in a container.
pub(super) fn runtime(settings: &Settings) -> Result<Runtime, Error> {
    let text = |name: &str| settings.text(name);
    let (image, container, conda) = (
        text("docker_image"),
        text("docker_container"),
        text("conda_env"),
    );
    let executable = text("executable");
    let name = match text("runtime") {
        Some(name) => name.to_lowercase().split_whitespace().collect::<String>(),
        None if image.is_some() || container.is_some() => "docker".to_owned(),
        None if conda.is_some() => "conda".to_owned(),
        None => "local".to_owned(),
    };
    let failed = |message: &str| Err(thrown(message));
    match name.as_str() {
        "docker" => {
            if conda.is_some() {
                return failed("The '_conda_env' option cannot be used with Docker runtime");
            }
            if executable.is_some() {
                return failed(
                    "The '_executable' option cannot be used with Docker runtime; use \
                     '_container_rscript' for the container-side Rscript path",
                );
            }
            match (&image, &container) {
                (Some(_), Some(_)) => {
                    return failed(
                        "Docker runtime accepts either '_docker_image' or '_docker_container', \
                         not both",
                    );
                }
                (None, None) => {
                    return failed(
                        "Docker runtime requires '_docker_image'/'nfR.docker_image' or \
                         '_docker_container'/'nfR.docker_container'",
                    );
                }
                _ => {}
            }
            if container.is_some() && !settings.list("docker_mounts").is_empty() {
                return failed(
                    "The '_docker_mounts' option can only be used with '_docker_image' / docker \
                     run; running containers must already have required mounts",
                );
            }
        }
        "conda" => {
            if executable.is_some() {
                return failed(
                    "The '_executable' and '_conda_env' options cannot be used together",
                );
            }
            if conda.is_none() {
                return failed("Conda runtime requires '_conda_env' or 'nfR.conda_env'");
            }
        }
        "local" if conda.is_some() => {
            return failed("The '_conda_env' option requires Conda runtime");
        }
        "local" => {}
        other => {
            return Err(thrown(format!(
                "Invalid _runtime value: {other}. Expected 'local', 'conda' or 'docker'"
            )));
        }
    }
    if let Some(conda) = conda {
        return Err(thrown(format!(
            "uwe runs R in a container and cannot use the conda environment {conda}; set \
             nfR.docker_image to an image with R and nanoarrow instead"
        )));
    }
    if let Some(container) = container {
        return Err(thrown(format!(
            "uwe runs each nf-r-ipc call in a new container and cannot use the running container \
             {container}; set nfR.docker_image instead"
        )));
    }
    let program = match name.as_str() {
        "docker" => text("container_rscript"),
        _ => executable.clone(),
    };
    let program = program.unwrap_or("Rscript".to_owned());
    let optional = |value: Option<String>| value.map_or(Value::Null, Value::string);
    let mut shown = vec![
        ("runtime".to_owned(), Value::string(name.as_str())),
        ("executable".to_owned(), optional(executable)),
        ("conda_env".to_owned(), Value::Null),
        ("r_libs".to_owned(), optional(text("r_libs"))),
        (
            "command".to_owned(),
            Value::list(vec![Value::string(program.as_str())]),
        ),
    ];
    if name == "docker" {
        let list =
            |name: &str| Value::list(settings.list(name).into_iter().map(Value::string).collect());
        shown.extend([
            (
                "docker_executable".to_owned(),
                optional(text("docker_executable")),
            ),
            ("docker_image".to_owned(), optional(image.clone())),
            ("docker_container".to_owned(), Value::Null),
            ("docker_args".to_owned(), list("docker_args")),
            ("docker_mounts".to_owned(), list("docker_mounts")),
            (
                "docker_workdir".to_owned(),
                optional(text("docker_workdir")),
            ),
            (
                "container_rscript".to_owned(),
                Value::string(program.as_str()),
            ),
        ]);
    }
    Ok(Runtime {
        name,
        image: image.unwrap_or(IMAGE.to_owned()),
        program,
        shown,
    })
}
