//! Workflow submission settings independent of command-line presentation.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Map, Value};
use workflow_graph::{CancellationToken, Language};

use crate::Failure;
use crate::arguments::{Arguments, Resume};
use crate::setup::{Defaults, Setup};

/// Configure with `new` or `default`, then assign the fields needed by the host.
/// Executor credentials and backend configuration belong in `setup`.
#[non_exhaustive]
pub struct Settings {
    pub target: PathBuf,
    pub language: Option<Language>,
    /// Typed input object. Use absolute paths for local file values.
    pub inputs: Option<Value>,
    /// Typed parameters override matching keys in `inputs`.
    pub params: Map<String, Value>,
    pub profiles: Vec<String>,
    pub configs: Vec<PathBuf>,
    /// Optional iunctus configuration file, separate from workflow config files.
    pub config_file: Option<PathBuf>,
    /// `None` uses the configured default, then local containers.
    pub executor: Option<String>,
    /// Supplied setup skips global configuration loading and defaults.
    pub setup: Option<Setup>,
    pub resume: Option<Resume>,
    /// Reuses and stores cached results when true, the default.
    pub cache: bool,
    pub targets: Vec<String>,
    pub until: Vec<String>,
    pub omit_from: Vec<String>,
    pub forcerun: Vec<String>,
    pub forceall: bool,
    pub default_image: Option<String>,
    /// Overrides the network policy from `setup` or configuration files.
    pub network: Option<task_runner::NetworkRule>,
    /// Maximum CPUs per task, at least one; backend allocation may be smaller.
    pub cpus: Option<u32>,
    /// Maximum memory per task, at least 6 MiB, including tasks without a memory request.
    pub memory_bytes: Option<u64>,
    pub max_pending: Option<usize>,
    /// Machine retries, defaulting to configuration or two retries.
    pub retries: Option<u32>,
    /// Initial retry delay, defaulting to configuration or one second.
    pub retry_delay: Option<Duration>,
    /// Completion cancels a child token and leaves this host token untouched.
    pub cancel: CancellationToken,
    pub output_dir: Option<PathBuf>,
    pub database: Option<PathBuf>,
    /// Local folder or writable storage address for cached results.
    pub store: Option<String>,
    /// The folder of downloads, scratch folders and input digests, like `[cache] folder` of
    /// the config file. It holds for the whole process, so the first run that names one sets it.
    pub cache_folder: Option<PathBuf>,
    // The CLI retains its input syntax and presentation without exposing them to hosts.
    pub(crate) cli: Option<Arguments>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            target: PathBuf::new(),
            language: None,
            inputs: None,
            params: Map::new(),
            profiles: Vec::new(),
            configs: Vec::new(),
            config_file: None,
            executor: None,
            setup: None,
            resume: None,
            cache: true,
            targets: Vec::new(),
            until: Vec::new(),
            omit_from: Vec::new(),
            forcerun: Vec::new(),
            forceall: false,
            default_image: None,
            network: None,
            cpus: None,
            memory_bytes: None,
            max_pending: None,
            retries: None,
            retry_delay: None,
            cancel: CancellationToken::new(),
            output_dir: None,
            database: None,
            store: None,
            cache_folder: None,
            cli: None,
        }
    }
}

impl Settings {
    pub fn new(target: impl Into<PathBuf>) -> Self {
        Self {
            target: target.into(),
            ..Self::default()
        }
    }

    pub(crate) fn prepare(
        mut self,
    ) -> Result<(Arguments, Option<Setup>, Option<tempfile::NamedTempFile>), Failure> {
        let cli = self.cli.is_some();
        if self.cpus == Some(0) || self.memory_bytes.is_some_and(|bytes| bytes < 6 << 20) {
            return Err(Failure::usage(
                "iunctus: resource limits require at least one CPU and 6 MiB of memory",
            ));
        }
        let mut arguments = self.cli.take().unwrap_or_default();
        arguments.target = (!self.target.as_os_str().is_empty()).then_some(self.target);
        arguments.language = self.language;
        arguments.profiles = (!self.profiles.is_empty()).then_some(self.profiles);
        arguments.configs = self.configs;
        arguments.config_file = self.config_file;
        arguments.executor = self.executor;
        arguments.resume = self.resume;
        arguments.no_cache = !self.cache;
        arguments.targets = self.targets;
        arguments.until = self.until;
        arguments.omit_from = self.omit_from;
        arguments.forcerun = self.forcerun;
        arguments.forceall = self.forceall;
        arguments.default_image = self.default_image;
        arguments.max_pending = self.max_pending;
        arguments.machine_retries = self.retries;
        arguments.retry_delay_ms = self
            .retry_delay
            .map(|delay| u64::try_from(delay.as_millis()).unwrap_or(u64::MAX));
        arguments.output_dir = self.output_dir;
        arguments.database = self.database;
        arguments.store = self.store;
        if let Some(folder) = self.cache_folder {
            crate::use_cache_folder(folder);
        }
        if !cli && self.setup.is_none() {
            Defaults::load(&arguments)?.apply("run", &mut arguments);
        }
        let file = if !cli && (self.inputs.is_some() || !self.params.is_empty()) {
            let mut values = self.inputs.unwrap_or_else(|| Value::Object(Map::new()));
            let object = values
                .as_object_mut()
                .ok_or_else(|| Failure::usage("iunctus: embedded inputs must be a JSON object"))?;
            object.extend(self.params);
            let file = tempfile::Builder::new()
                .prefix("iunctus-inputs-")
                .suffix(".json")
                .tempfile()
                .map_err(|error| Failure::setup(format!("iunctus: {error}")))?;
            serde_json::to_writer(file.as_file(), &values)
                .map_err(|error| Failure::setup(format!("iunctus: {error}")))?;
            arguments.inputs = Some(file.path().to_owned());
            Some(file)
        } else {
            None
        };
        Ok((arguments, self.setup, file))
    }
}
