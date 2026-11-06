//! Builds AWS Batch executors from `[executor.aws-batch]`, with object-store staging and the uwe
//! helper for the jobs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::time::Duration;

use aws_batch_executor::{BatchBackend, Client, Settings};
use config_file::{ConfigError, Table};
use content_store::Store;
use task_runner::{Runner, RunnerExecutor};

/// The least time between two calls to AWS: Nextflow's default limit of 50 calls a second.
const PACE: Duration = Duration::from_millis(20);

/// The settings of `[executor.aws-batch]`: those of the jobs, of the staging root, and the
/// binary that uwe uploads for the jobs.
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

/// An executor that runs each task as an AWS Batch job, with its attempts in
/// `<scratch>/aws-batch`. Tasks without an image get `default_image`.
pub fn executor(
    store: Store,
    scratch: &Path,
    run: &str,
    default_image: Option<String>,
    setup: &crate::setup::Setup,
) -> Result<RunnerExecutor<crate::conda_backend::Remote<BatchBackend>>, String> {
    let config = &setup.executors.aws_batch;
    let stage = config.staging.stage(&setup.storage)?.ok_or(
        "AWS Batch jobs share no folders with this machine; set staging = \"object-store\" and \
         staging_root = \"s3://bucket/folder\" in [executor.aws-batch]",
    )?;
    let variable = |name: &str| std::env::var(name).ok();
    let mut settings = config.settings.clone();
    let region = settings.region(variable)?;
    settings.region = Some(region.clone());
    let client = Client::new(&region, settings.credentials(variable), PACE);
    let upload = match (&settings.helper, &config.helper_file) {
        (None, Some(file)) => Some(
            executor::binary::Binary::open(file)
                .map_err(|error| format!("{error}, or set helper_path or helper_url"))?,
        ),
        _ => None,
    };
    let scratch = scratch.join("aws-batch");
    let mut backend = BatchBackend::new(client, settings, scratch.clone(), stage, upload)?;
    backend.staging_time = config.staging.margin();
    // The jobs read the inputs from the staging root, so they are copied there.
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
        Config::read(&section.table("aws-batch")?.expect("a table"))
    }

    #[test]
    fn reads_batch_config() {
        let text = "[executor.aws-batch]\nqueue = 'q'\nstaging = 'object-store'\n\
                    staging_root = 's3://bucket/uwe'\nhelper_file = '/opt/uwe-arm64'\n";
        let config = read(text).expect("a config");
        assert_eq!(config.settings.queue.as_deref(), Some("q"));
        assert_eq!(config.staging.root.as_deref(), Some("s3://bucket/uwe"));
        assert_eq!(config.helper_file, Some(PathBuf::from("/opt/uwe-arm64")));
        let named = "[executor.aws-batch]\nhelper_path = '/opt/uwe'\n";
        assert_eq!(read(named).expect("a config").helper_file, None);
        let both = format!("{named}helper_file = '/opt/uwe'\n");
        assert!(read(&both).is_err(), "two places for the helper");
        assert!(read("[executor.aws-batch]\nqueues = 'q'\n").is_err());
    }
}
