//! Nextflow config files: evaluates them into one tree of settings, with includes, profiles
//! and params.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod file;
mod flat;
mod process;
pub mod tree;

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::value::Value;

pub use process::ProcessSettings;

/// What the command line gives the config.
#[derive(Clone, Debug)]
pub struct Options {
    /// The profiles of `-profile`, in order; `None` applies a `standard` profile if one exists.
    pub profiles: Option<Vec<String>>,
    /// The params of the command line and the params file, as a map.
    pub params: Value,
    /// The directory of the main script.
    pub project_dir: String,
    /// The directory the run starts in.
    pub launch_dir: String,
}

/// The merged settings of all config files.
#[derive(Clone, Debug)]
pub struct Config {
    /// Nested maps; `params`, `process`, `env` and `executor` always exist.
    pub tree: Value,
}

impl Default for Config {
    /// A config without settings, as when no config file exists.
    fn default() -> Self {
        let tree = tree::map();
        for scope in ["env", "executor", "params", "process"] {
            tree::assign(&tree, &[scope.into()], tree::map()).expect("the top level is a map");
        }
        Config { tree }
    }
}

impl Config {
    /// The value of a setting, like `["docker", "enabled"]`.
    pub fn get(&self, path: &[&str]) -> Option<Value> {
        tree::get(&self.tree, path)
    }

    /// One `a.b.c = value` line per setting, as `nextflow config -flat` prints them.
    pub fn flat(&self) -> String {
        flat::flat(&self.tree)
    }

    /// The final params: the config params with the command-line values over them.
    pub fn params(&self) -> Value {
        self.get(&["params"]).unwrap_or_else(tree::map)
    }
}

/// Why the config could not be loaded.
#[derive(Debug)]
pub enum ConfigError {
    /// Syntax and name errors, each with its file and position.
    Invalid { messages: Vec<String> },
    /// An error while a file ran.
    Failed { file: PathBuf, error: Error },
}

impl ConfigError {
    /// Whether the cause is a feature that iunctus does not support yet.
    pub fn unsupported(&self) -> bool {
        match self {
            ConfigError::Invalid { messages } => messages
                .iter()
                .any(|message| message.contains("not supported yet")),
            ConfigError::Failed { error, .. } => error.unsupported(),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Invalid { messages } => f.write_str(&messages.join("\n")),
            ConfigError::Failed { error, .. } if error.unsupported() => write!(f, "{error}"),
            ConfigError::Failed { file, error } => write!(
                f,
                "Unable to parse config file: '{}'\n  {error}",
                file.display()
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

/// The config files Nextflow reads for a run, lowest priority first: `nextflow.config` in the
/// project directory, in the launch directory if that is another file, then the `-c` files.
/// The home config `$NXF_HOME/config` is left out, so that runs do not depend on it.
pub fn files(project_dir: &Path, launch_dir: &Path, extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let project = project_dir.join("nextflow.config");
    if project.is_file() {
        files.push(project.clone());
    }
    let launch = launch_dir.join("nextflow.config");
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if launch.is_file() && !same(&launch, &project) {
        files.push(launch);
    }
    files.extend(extra.iter().map(|file| launch_dir.join(file)));
    files
}

/// Evaluates the files in order and merges their trees.
pub fn load(
    files: &[PathBuf],
    options: &Options,
    host: &mut dyn Host,
) -> Result<Config, ConfigError> {
    let mut evaluator = file::Evaluator::new(options, host);
    let merged = tree::map();
    for path in files {
        let tree = evaluator.top_level(path)?;
        tree::deep_merge(&merged, &tree);
    }
    if let Some(profiles) = &options.profiles
        && !matches!(profiles.as_slice(), [name] if name == "standard")
    {
        let unknown: Vec<&String> = profiles
            .iter()
            .filter(|name| !evaluator.declared_profiles.contains(*name))
            .collect();
        if let Some(name) = unknown.first() {
            return Err(ConfigError::Invalid {
                messages: vec![unknown_profile(name, &evaluator.declared_profiles)],
            });
        }
    }
    for scope in ["env", "executor", "params", "process"] {
        if tree::get(&merged, &[scope]).is_none() {
            let path = [scope.into()];
            tree::assign(&merged, &path, tree::map()).expect("the top level is a map");
        }
    }
    let params = tree::get(&merged, &["params"]).unwrap_or_else(tree::map);
    tree::deep_merge(&params, &options.params);
    if let Value::Map(params) = &params {
        for value in params.borrow().entries.values() {
            config_maps(value);
        }
    }
    Ok(Config { tree: merged })
}

fn unknown_profile(name: &str, declared: &BTreeSet<String>) -> String {
    let names: Vec<&str> = declared.iter().map(String::as_str).collect();
    format!(
        "Unknown configuration profile: '{name}'\n\nAvailable config profiles: {}",
        names.join(", ")
    )
}

/// Makes the maps in a value of the config, also those in lists, `nextflow.config.ConfigMap`,
/// as Nextflow gives them to scripts.
fn config_maps(value: &Value) {
    match value {
        Value::Map(map) => {
            map.borrow_mut().kind = nextflow_evaluator::value::MapKind::Config;
            for value in map.borrow().entries.values() {
                config_maps(value);
            }
        }
        Value::List(list) => {
            for item in &list.borrow().items {
                config_maps(item);
            }
        }
        _ => {}
    }
}
