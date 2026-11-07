//! Builds Azure Batch executors from `[executor.azure-batch]`, with object-store staging and the
//! uwe helper for the tasks.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use azure_batch_executor::{AzureBackend, Client, Credentials, Settings};
use config_file::{ConfigError, Table};
use content_store::Store;
use task_runner::{Runner, RunnerExecutor};

/// The settings of `[executor.azure-batch]`: those of the tasks, of the staging root, and the
/// binary that uwe uploads for the tasks.
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
        let helper_file = crate::setup::helper_file(table, settings.helper.is_some())?;
        Ok(Config {
            settings,
            staging,
            helper_file,
        })
    }
}

/// An executor that runs each task as an Azure Batch task, with its attempts in
/// `<scratch>/azure-batch`. Tasks without an image get `default_image`.
pub fn executor(
    store: Store,
    scratch: &Path,
    run: &str,
    default_image: Option<String>,
    setup: &crate::setup::Setup,
    secrets: &config_file::SecretValues,
) -> Result<RunnerExecutor<crate::conda_backend::Remote<AzureBackend>>, String> {
    let config = &setup.executors.azure_batch;
    let stage = config.staging.stage(&setup.storage)?.ok_or(
        "Azure Batch tasks share no folders with this machine; set staging = \"object-store\" \
         and staging_root = \"az://container/folder\" in [executor.azure-batch]",
    )?;
    let variable = |name: &str| std::env::var(name).ok();
    let settings = config.settings.clone();
    let (endpoint, account) = address(&settings, variable)?;
    let credentials = Credentials::find(&settings, &account, |name| secrets.get(name))?;
    let api = settings.api_version.clone();
    let client = Client::new(endpoint, account, api, credentials);
    let upload = match (&settings.helper, &config.helper_file) {
        (None, Some(file)) => Some(
            executor::binary::Binary::open(file)
                .map_err(|error| format!("{error}, or set helper_path or helper_url"))?,
        ),
        _ => None,
    };
    let scratch = scratch.join("azure-batch");
    let mut backend = AzureBackend::new(client, settings, scratch.clone(), stage, upload)?;
    backend.staging_time = config.staging.margin();
    // The tasks read the inputs from the staging root, so they are copied there.
    let backend = crate::conda_backend::Remote::new(backend, store.clone(), &scratch, run, setup);
    let settings = crate::runner_settings(setup.storage.clone(), true);
    let mut runner = Runner::new(store, backend, run, settings);
    runner.references(setup.references.clone());
    Ok(RunnerExecutor::new(runner, default_image))
}

/// The endpoint and account of Batch: the endpoint, else `https://<account>.<location>.batch.
/// azure.com`; the account, else `AZURE_BATCH_ACCOUNT_NAME`, else the first part of the host.
fn address(
    settings: &Settings,
    variable: impl Fn(&str) -> Option<String>,
) -> Result<(String, String), String> {
    let named = (settings.account.clone())
        .or_else(|| variable("AZURE_BATCH_ACCOUNT_NAME").filter(|name| !name.is_empty()));
    let endpoint = match (&settings.endpoint, &named, &settings.location) {
        (Some(endpoint), _, _) => endpoint.clone(),
        (None, Some(account), Some(location)) => {
            format!("https://{account}.{location}.batch.azure.com")
        }
        _ => {
            return Err(
                "Azure Batch needs an endpoint: set `endpoint`, or `account` and \
                        `location`, in [executor.azure-batch], or azure.batch.endpoint in the \
                        Nextflow config"
                    .into(),
            );
        }
    };
    let host = endpoint.split("://").nth(1).unwrap_or_default();
    let from_host = host.split(['.', ':', '/']).next().unwrap_or_default();
    let account = named.unwrap_or_else(|| from_host.to_owned());
    if account.is_empty() {
        return Err(
            "Azure Batch needs an account name: set `account` in [executor.azure-batch]".into(),
        );
    }
    Ok((endpoint, account))
}

#[cfg(test)]
mod tests {
    use super::*;
    use config_file::ConfigFile;

    fn read(text: &str) -> Result<Config, ConfigError> {
        let config = ConfigFile::parse(text, Path::new("uwe.toml"))?;
        let section = config.section("executor")?.expect("a section");
        Config::read(&section.table("azure-batch")?.expect("a table"))
    }

    #[test]
    fn reads_batch_config() {
        let text = "[executor.azure-batch]\naccount = 'lab'\nstaging = 'object-store'\n\
                    staging_root = 'az://uwe/staging'\nhelper_file = '/opt/uwe-arm64'\n";
        let config = read(text).expect("a config");
        assert_eq!(config.settings.account.as_deref(), Some("lab"));
        assert_eq!(config.staging.root.as_deref(), Some("az://uwe/staging"));
        assert_eq!(config.helper_file, Some(PathBuf::from("/opt/uwe-arm64")));
        assert!(read("[executor.azure-batch]\nqueue = 'q'\n").is_err());
    }

    #[test]
    fn finds_batch_address() {
        let settings = Settings {
            account: Some("lab".into()),
            location: Some("westeurope".into()),
            ..Settings::default()
        };
        let found = address(&settings, |_| None);
        let expected = (
            "https://lab.westeurope.batch.azure.com".to_owned(),
            "lab".to_owned(),
        );
        assert_eq!(found, Ok(expected));
        let named = Settings {
            endpoint: Some("https://other.eastus.batch.azure.com".into()),
            ..Settings::default()
        };
        assert_eq!(
            address(&named, |_| None).map(|(_, account)| account),
            Ok("other".into())
        );
        assert!(address(&Settings::default(), |_| None).is_err());
    }
}
