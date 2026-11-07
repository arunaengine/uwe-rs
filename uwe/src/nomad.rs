//! Builds Nomad executors from `[executor.nomad]`, with shared-path or object-store staging and
//! the uwe helper for the jobs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use config_file::{ConfigError, Table};
use content_store::Store;
use nomad_executor::{ADDRESS, Client, NomadBackend, Settings};
use task_runner::{Runner, RunnerExecutor};

/// The settings of `[executor.nomad]`: those of the jobs, of the staging root, and the binary
/// that uwe uploads for the jobs.
#[derive(Clone, Debug, Default)]
pub struct Config {
    pub settings: Settings,
    pub staging: crate::staging::Settings,
    /// Without `helper_path` and `helper_url`: `helper_file`, or else the running binary.
    pub helper_file: Option<PathBuf>,
}

impl Config {
    pub fn read(table: &Table<'_>) -> Result<Config, ConfigError> {
        let mut keys = Settings::KEYS.to_vec();
        keys.push("helper_file");
        keys.extend(crate::staging::Settings::KEYS);
        table.only(&keys)?;
        let settings = Settings::read(table)?;
        let staging = crate::staging::Settings::read(table, &["object-store"])?;
        let helper_file = match staging.mode {
            crate::staging::Mode::ObjectStore => {
                crate::setup::helper_file(table, settings.helper.is_some())?
            }
            _ => None,
        };
        Ok(Config {
            settings,
            staging,
            helper_file,
        })
    }
}

/// An executor that runs each task as a Nomad batch job, with its attempts in
/// `<scratch>/nomad`. Tasks without an image get `default_image`.
pub fn executor(
    store: Store,
    scratch: &Path,
    run: &str,
    default_image: Option<String>,
    setup: &crate::setup::Setup,
) -> Result<RunnerExecutor<crate::conda_backend::Remote<NomadBackend>>, String> {
    let config = &setup.executors.nomad;
    let stage = config.staging.stage(&setup.storage)?;
    let variable = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let settings = config.settings.clone();
    let address = (settings.address.clone())
        .or_else(|| variable("NOMAD_ADDR"))
        .unwrap_or_else(|| ADDRESS.into());
    let token = match &settings.token {
        Some(token) => Some(token.reveal()?),
        None => variable("NOMAD_TOKEN"),
    };
    let namespace = settings
        .namespace
        .clone()
        .or_else(|| variable("NOMAD_NAMESPACE"));
    let region = settings.region.clone().or_else(|| variable("NOMAD_REGION"));
    let client = Client::new(&address, token, namespace, region);
    let upload = match (&stage, &settings.helper, &config.helper_file) {
        (Some(_), None, Some(file)) => Some(
            executor::binary::Binary::open(file)
                .map_err(|error| format!("{error}, or set helper_path or helper_url"))?,
        ),
        _ => None,
    };
    let scratch = scratch.join("nomad");
    let mut backend = NomadBackend::new(client, settings, scratch.clone(), stage, upload)?;
    backend.staging_time = config.staging.margin();
    // The jobs read the inputs from the store or the staging root, so they are copied there.
    let backend = crate::conda_backend::Remote::new(backend, store.clone(), &scratch, run, setup);
    let settings = crate::runner_settings(setup.storage.clone(), true);
    let mut runner = Runner::new(store, backend, run, settings);
    runner.references(setup.references.clone());
    Ok(RunnerExecutor::new(runner, default_image))
}

#[cfg(test)]
mod tests {
    use super::*;
    use config_file::ConfigFile;

    fn read(text: &str) -> Result<Config, ConfigError> {
        let config = ConfigFile::parse(text, Path::new("uwe.toml"))?;
        let section = config.section("executor")?.expect("a section");
        Config::read(&section.table("nomad")?.expect("a table"))
    }

    #[test]
    fn reads_nomad_config() {
        let text = "[executor.nomad]\nnamespace = 'lab'\nstaging = 'object-store'\n\
                    staging_root = 's3://bucket/uwe'\nhelper_file = '/opt/uwe-arm64'\n";
        let config = read(text).expect("a config");
        assert_eq!(config.settings.namespace.as_deref(), Some("lab"));
        assert_eq!(config.staging.root.as_deref(), Some("s3://bucket/uwe"));
        assert_eq!(config.helper_file, Some(PathBuf::from("/opt/uwe-arm64")));
        let shared =
            read("[executor.nomad]\nvolumes = [{ name = 'scratch', path = '/scratch' }]\n");
        assert_eq!(shared.expect("a config").helper_file, None);
        assert!(read("[executor.nomad]\nprivileged = true\n").is_err());
    }
}
