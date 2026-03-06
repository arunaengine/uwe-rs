//! Nextflow config files: evaluates them into one tree of settings, with includes, profiles
//! and params, following the rules of `docs/research/nextflow-config.md`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod file;
pub mod tree;

use std::fmt;
use std::path::PathBuf;

use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::value::Value;

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

impl Config {
    /// The value of a setting, like `["docker", "enabled"]`.
    pub fn get(&self, path: &[&str]) -> Option<Value> {
        tree::get(&self.tree, path)
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
            ConfigError::Failed { error, .. } => matches!(error, Error::Unsupported { .. }),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Invalid { messages } => f.write_str(&messages.join("\n")),
            ConfigError::Failed { file, error } => write!(
                f,
                "Unable to parse config file: '{}'\n  {error}",
                file.display()
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

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
    for scope in ["env", "executor", "params", "process"] {
        if tree::get(&merged, &[scope]).is_none() {
            let path = [scope.into()];
            tree::assign(&merged, &path, tree::map()).expect("the top level is a map");
        }
    }
    let params = tree::get(&merged, &["params"]).unwrap_or_else(tree::map);
    tree::deep_merge(&params, &options.params);
    Ok(Config { tree: merged })
}
