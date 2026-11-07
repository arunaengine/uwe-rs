//! Builds Flyte executors from `[executor.flyte]`, with object-store staging and the uwe helper
//! that Flyte downloads for each run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use config_file::{ConfigError, Table};
use content_store::Store;
use flyte_executor::{ApiKey, Client, FlyteBackend, KEY_VARIABLES, Project, Settings, address};
use task_runner::{Runner, RunnerExecutor};

/// The settings of `[executor.flyte]`: those of the runs, of the staging root, and the binary
/// that uwe uploads for the runs.
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

/// An executor that runs each task as a Flyte run, with its attempts in `<scratch>/flyte`.
/// Tasks without an image get `default_image`.
pub fn executor(
    store: Store,
    scratch: &Path,
    run: &str,
    default_image: Option<String>,
    setup: &crate::setup::Setup,
) -> Result<RunnerExecutor<crate::conda_backend::Remote<FlyteBackend>>, String> {
    let config = &setup.executors.flyte;
    let stage = config.staging.stage(&setup.storage)?.ok_or(
        "Flyte runs share no folders with this machine; set staging = \"object-store\" and \
         staging_root = \"s3://bucket/folder\" in [executor.flyte], or an object-store workDir",
    )?;
    let variable = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let settings = config.settings.clone();
    let key = match &settings.api_key {
        Some(secret) => Some(secret.reveal().map_err(|error| error.to_string())?),
        None => KEY_VARIABLES.iter().find_map(|name| variable(name)),
    };
    let key = key.map(|key| ApiKey::parse(&key)).transpose()?;
    let endpoint = (settings.endpoint.clone())
        .or_else(|| variable("_U_EP_OVERRIDE"))
        .or_else(|| variable("FLYTE_ADMIN_ENDPOINT"))
        .or_else(|| key.as_ref().map(|key| key.endpoint.clone()));
    let org = (settings.org.clone())
        .or_else(|| variable("_U_ORG_NAME"))
        .or_else(|| key.as_ref().map(|key| key.org.clone()));
    let project =
        (settings.project.clone()).or_else(|| variable("FLYTE_INTERNAL_EXECUTION_PROJECT"));
    let domain = (settings.domain.clone()).or_else(|| variable("FLYTE_INTERNAL_EXECUTION_DOMAIN"));
    let named = [
        ("endpoint", endpoint),
        ("org", org),
        ("project", project),
        ("domain", domain),
    ];
    let named = named.map(|(name, value)| (name, value.filter(|value| !value.is_empty())));
    let [
        (_, Some(endpoint)),
        (_, Some(org)),
        (_, Some(project)),
        (_, Some(domain)),
    ] = named.clone()
    else {
        let missing: Vec<&str> = (named.iter())
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| *name)
            .collect();
        return Err(format!(
            "Executor `flyte` is missing required settings: {} -- set them in [executor.flyte] \
             or the `flyte` scope; an API key in FLYTE_API_KEY gives the endpoint and org",
            missing.join(", ")
        ));
    };
    let client = Client::new(&address(&endpoint), key);
    // A pipeline may pick Flyte without an [executor.flyte] that names the helper.
    let file = (config.helper_file.clone()).or_else(|| std::env::current_exe().ok());
    let upload = match (&settings.helper, file) {
        (None, Some(file)) => Some(
            executor::binary::Binary::open(&file)
                .map_err(|error| format!("{error}, or set helper_path or helper_url"))?,
        ),
        _ => None,
    };
    let project = Project {
        org,
        project,
        domain,
    };
    let scratch = scratch.join("flyte");
    let mut backend = FlyteBackend::new(client, project, settings, scratch.clone(), stage, upload)?;
    backend.staging_time = config.staging.margin();
    // The runs read the inputs from the staging root, so they are copied there.
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
        Config::read(&section.table("flyte")?.expect("a table"))
    }

    #[test]
    fn reads_flyte_config() {
        let text = "[executor.flyte]\nproject = 'flows'\nstaging = 'object-store'\n\
                    staging_root = 's3://bucket/uwe'\nhelper_file = '/opt/uwe-arm64'\n";
        let config = read(text).expect("a config");
        assert_eq!(config.settings.project.as_deref(), Some("flows"));
        assert_eq!(config.staging.root.as_deref(), Some("s3://bucket/uwe"));
        assert_eq!(config.helper_file, Some(PathBuf::from("/opt/uwe-arm64")));
        let named = "[executor.flyte]\nhelper_path = '/opt/uwe'\n";
        assert_eq!(read(named).expect("a config").helper_file, None);
        assert!(read("[executor.flyte]\nauth_command = 'x'\n").is_err());
    }
}
