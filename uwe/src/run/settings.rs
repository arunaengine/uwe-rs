//! Workflow submission settings independent of command-line presentation.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Map, Value};
use workflow_graph::{CancellationToken, Language};

use super::Listener;
use crate::Failure;
use crate::arguments::{Arguments, Resume};
use crate::setup::{Defaults, Setup};

/// Configure with `new` or `default`, then assign the fields needed by the host.
/// Executor credentials and backend configuration belong in `setup`.
#[non_exhaustive]
pub struct Settings {
    pub preset: Option<String>,
    pub hosted: Option<crate::hosted::Hosted>,
    pub strict_provenance: bool,
    pub target: PathBuf,
    /// The tag, branch or commit of a repository `target`, like `-r` of `uwe run`. A local
    /// target or a preset takes none.
    pub revision: Option<String>,
    /// Resolves a remembered branch of a repository `target` again, like `-latest`.
    pub latest: bool,
    pub language: Option<Language>,
    /// Typed input object. Use absolute paths for local file values.
    pub inputs: Option<Value>,
    /// Typed parameters override matching keys in `inputs`.
    pub params: Map<String, Value>,
    pub profiles: Vec<String>,
    pub configs: Vec<PathBuf>,
    /// Optional uwe configuration file, separate from workflow config files.
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
    /// the config file. Embedded submissions retain their own cache scope.
    pub cache_folder: Option<PathBuf>,
    /// The run id chosen by the host instead of a new one: 1 to 64 ASCII letters, digits, `-`
    /// or `_`, starting and ending with a letter or digit, and new to the run database.
    pub run_id: Option<String>,
    /// Hears the run and its tasks as they change; see [`Listener`] for what it must do.
    pub listener: Option<Listener>,
    // The CLI retains its input syntax and presentation without exposing them to hosts.
    pub(crate) cli: Option<Arguments>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            preset: None,
            hosted: None,
            strict_provenance: false,
            target: PathBuf::new(),
            revision: None,
            latest: false,
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
            run_id: None,
            listener: None,
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
        let preset = self
            .preset
            .take()
            .or_else(|| self.cli.as_ref()?.preset.clone());
        if let Some(hosted) = &self.hosted {
            let root = hosted.check(preset.as_deref())?;
            for (given, expected) in [
                (&mut self.cache_folder, root.join("cache")),
                (&mut self.database, root.join("runs.sqlite")),
                (&mut self.output_dir, root.join("outputs")),
            ] {
                if given.as_ref().is_some_and(|path| path != &expected) {
                    return Err(Failure::usage(
                        "hosted paths must stay inside the tenant root",
                    ));
                }
                *given = Some(expected);
            }
        }
        if self.cpus == Some(0) || self.memory_bytes.is_some_and(|bytes| bytes < 6 << 20) {
            return Err(Failure::usage(
                "uwe: resource limits require at least one CPU and 6 MiB of memory",
            ));
        }
        if self
            .revision
            .as_ref()
            .is_some_and(|revision| revision.is_empty() || revision.starts_with('-'))
        {
            return Err(Failure::usage(
                "uwe: the revision needs a tag, branch or commit",
            ));
        }
        let mut arguments = self.cli.take().unwrap_or_default();
        arguments.preset = preset;
        arguments.target = (!self.target.as_os_str().is_empty()).then_some(self.target);
        // The CLI keeps its own `-r` and `-latest` in `cli`.
        arguments.revision = self.revision.or(arguments.revision);
        arguments.latest |= self.latest;
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
        let mut file = if !cli && (self.inputs.is_some() || !self.params.is_empty()) {
            let mut values = self.inputs.unwrap_or_else(|| Value::Object(Map::new()));
            let object = values
                .as_object_mut()
                .ok_or_else(|| Failure::usage("uwe: embedded inputs must be a JSON object"))?;
            object.extend(self.params);
            let file = tempfile::Builder::new()
                .prefix("uwe-inputs-")
                .suffix(".json")
                .tempfile()
                .map_err(|error| Failure::setup(format!("uwe: {error}")))?;
            serde_json::to_writer(file.as_file(), &values)
                .map_err(|error| Failure::setup(format!("uwe: {error}")))?;
            arguments.inputs = Some(file.path().to_owned());
            Some(file)
        } else {
            None
        };
        let prepared = if arguments.preset.is_some() {
            crate::catalogue::Catalogue::load(arguments.config_file.as_deref())?
                .prepare(&mut arguments)?
        } else {
            None
        };
        if let (Some(hosted), Some(prepared)) = (&self.hosted, &prepared) {
            let bytes = std::fs::read(prepared.inputs.path())
                .map_err(|error| Failure::setup(format!("uwe: {error}")))?;
            let values = serde_json::from_slice(&bytes)
                .map_err(|error| Failure::setup(format!("uwe: {error}")))?;
            hosted.authorize_values(&values)?;
        }
        if (!cli || prepared.is_some()) && self.setup.is_none() {
            Defaults::load(&arguments)?.apply("run", &mut arguments);
        }
        if let Some(folder) = self.cache_folder {
            crate::use_cache_folder(folder);
        }
        if let Some(prepared) = prepared {
            let setup = match self.setup.take() {
                Some(setup) => setup,
                None => Setup::load(&arguments)?,
            };
            self.setup = Some(Setup {
                references: prepared.references,
                ..setup
            });
            file = Some(prepared.inputs);
        }
        if let Some(hosted) = self.hosted {
            let mut setup = match self.setup.take() {
                Some(setup) => setup,
                None => Setup::load(&arguments)?,
            };
            let backend = arguments.executor.as_deref().unwrap_or("local");
            if (backend == "kubernetes" && setup.executors.kubernetes_staging.credentials)
                || (backend == "slurm" && setup.executors.slurm_staging.credentials)
            {
                return Err(Failure::usage(
                    "hosted tasks require attempt-scoped staging access",
                ));
            }
            if backend == "local"
                && (setup.executors.local_cpus.is_none() || setup.executors.local_memory.is_none())
            {
                return Err(Failure::usage(
                    "hosted local execution requires explicit CPU and memory capacity",
                ));
            }
            setup.hosted = true;
            setup.argo_policy = argo_runtime::Policy {
                host_roots: Some(hosted.host_roots),
                volumes: hosted.volumes,
            };
            setup.argo_locks = hosted.locks;
            if let Some(namespace) = hosted.namespace {
                if setup
                    .cluster
                    .as_ref()
                    .is_some_and(|cluster| cluster.namespace() != namespace)
                {
                    return Err(Failure::usage(
                        "hosted cluster namespace differs from its tenant policy",
                    ));
                }
                arguments.kubernetes_namespace = Some(namespace);
            }
            self.setup = Some(setup);
        }
        Ok((arguments, self.setup, file))
    }
}

/// Whether a host-chosen run id is safe in folder names, database keys and URLs: 1 to 64
/// ASCII letters, digits, `-` or `_`, starting and ending with a letter or a digit.
pub(crate) fn check_id(id: &str) -> Result<(), Failure> {
    let edge = |byte: Option<&u8>| byte.is_some_and(u8::is_ascii_alphanumeric);
    let bytes = id.as_bytes();
    if bytes.len() <= 64
        && edge(bytes.first())
        && edge(bytes.last())
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(byte))
    {
        return Ok(());
    }
    Err(Failure::usage(format!(
        "uwe: the run id {id:?} is not allowed; use 1 to 64 ASCII letters, digits, `-` or `_`, starting and ending with a letter or digit"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_preset_defaults() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        for (preset, explicit, expected) in [
            ("", None, "slurm"),
            ("executor='local'\n", None, "local"),
            ("executor='local'\n", Some("simulator"), "simulator"),
        ] {
            std::fs::write(&config, format!(
                "[executor]\ndefault='slurm'\n[workflows.demo]\nsource='team/pipeline'\nrevision='{}'\n{preset}",
                "a".repeat(40)
            )).unwrap();
            let arguments = Arguments {
                preset: Some("demo".into()),
                config_file: Some(config.clone()),
                executor: explicit.map(str::to_owned),
                ..Arguments::default()
            };
            let embedded = Settings {
                preset: arguments.preset.clone(),
                config_file: arguments.config_file.clone(),
                executor: arguments.executor.clone(),
                ..Settings::default()
            };
            for settings in [arguments.run_settings(), embedded] {
                let (resolved, _, _inputs) = settings.prepare().unwrap();
                assert_eq!(resolved.executor.as_deref(), Some(expected));
            }
        }
    }
}
