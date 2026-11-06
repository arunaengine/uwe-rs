//! Builds JARVICE executors from `[executor.jarvice]`, with the vault as shared path or with
//! object-store staging and the uwe helper for the jobs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use config_file::{ConfigError, Table};
use content_store::Store;
use jarvice_executor::{Client, JarviceBackend, Settings};
use task_runner::{Runner, RunnerExecutor};

/// The settings of `[executor.jarvice]`: those of the jobs, of the staging root, and the binary
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

/// An executor that runs each task as a JARVICE batch job, with its attempts in
/// `<scratch>/jarvice`. Tasks without an image get `default_image`.
pub fn executor(
    store: Store,
    scratch: &Path,
    run: &str,
    default_image: Option<String>,
    setup: &crate::setup::Setup,
) -> Result<RunnerExecutor<crate::conda_backend::Remote<JarviceBackend>>, String> {
    let config = &setup.executors.jarvice;
    let settings = config.settings.clone();
    let username = (settings.username.clone())
        .ok_or("-executor jarvice needs username and api_key in [executor.jarvice]")?;
    let key = (settings.api_key.as_ref())
        .ok_or("-executor jarvice needs api_key in [executor.jarvice]")?
        .reveal()
        .map_err(|error| error.to_string())?;
    let client = Client::new(&settings.api_url, username, key);
    let stage = config.staging.stage(&setup.storage)?;
    let upload = match (&stage, &settings.helper, &config.helper_file) {
        (Some(_), None, Some(file)) => Some(
            executor::binary::Binary::open(file)
                .map_err(|error| format!("{error}, or set helper_path or helper_url"))?,
        ),
        _ => None,
    };
    let scratch = scratch.join("jarvice");
    let mut backend = JarviceBackend::new(client, settings, scratch.clone(), stage, upload)?;
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
        Config::read(&section.table("jarvice")?.expect("a table"))
    }

    #[test]
    fn reads_jarvice_config() {
        let text = "[executor.jarvice]\nusername = 'lab'\nstaging = 'object-store'\n\
                    staging_root = 's3://bucket/uwe'\nhelper_file = '/opt/uwe-arm64'\n";
        let config = read(text).expect("a config");
        assert_eq!(config.settings.username.as_deref(), Some("lab"));
        assert_eq!(config.staging.root.as_deref(), Some("s3://bucket/uwe"));
        assert_eq!(config.helper_file, Some(PathBuf::from("/opt/uwe-arm64")));
        let shared = read("[executor.jarvice]\nvault = 'persistent'\n").expect("a config");
        assert_eq!(shared.helper_file, None);
        assert!(read("[executor.jarvice]\nprivileged = true\n").is_err());
    }
}
