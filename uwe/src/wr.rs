//! Builds wr executors from `[executor.wr]`: the manager's REST API with its token and CA, and
//! the jobs on shared folders or with object-store staging, as for the other schedulers.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use config_file::{ConfigError, Table};
use content_store::Store;
use slurm_executor::grid::GridBackend;
use task_runner::RunnerExecutor;
use wr_executor::{Settings, Wr};

/// The settings of `[executor.wr]`: those of the manager, and those of the jobs that the
/// scheduler executors share.
#[derive(Clone, Debug, Default)]
pub struct Config {
    pub settings: Settings,
    pub jobs: crate::grid::Config,
}

impl Config {
    pub fn read(table: &Table<'_>) -> Result<Config, ConfigError> {
        let mut keys = Settings::KEYS.to_vec();
        keys.extend(["container", "node_folder", "helper_file"]);
        keys.extend(["helper_path", "helper_url", "helper_sha256"]);
        keys.extend(crate::staging::Settings::KEYS);
        table.only(&keys)?;
        let staging = crate::staging::Settings::read(table, &["object-store"])?;
        if staging.credentials {
            let message = "must be presigned: the jobs get no secrets";
            return Err(table.invalid("staging_access", message));
        }
        let helper = crate::setup::helper(table)?;
        let helper_file = match staging.mode {
            crate::staging::Mode::ObjectStore => {
                crate::setup::helper_file(table, helper.is_some())?
            }
            _ => None,
        };
        let settings = Settings::read(table)?;
        let jobs = crate::grid::Config {
            container: table.text("container")?,
            staging,
            helper,
            helper_file,
            node_folder: table.text("node_folder")?,
            poll: Some(settings.poll),
            ..crate::grid::Config::default()
        };
        Ok(Config { settings, jobs })
    }
}

/// The executor of `-executor wr`, with its attempts in `<scratch>/wr`. The token and the CA
/// of the manager are in the home folder of the user who runs uwe, unless the settings say.
pub fn executor(
    store: Store,
    scratch: &Path,
    run: &str,
    default_image: Option<String>,
    setup: &crate::setup::Setup,
) -> Result<RunnerExecutor<crate::conda_backend::Remote<GridBackend<Wr>>>, String> {
    let config = &setup.executors.wr;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    // wr picks the port of a manager by the user who runs it.
    let uid = (std::fs::metadata("/proc/self").map(|metadata| metadata.uid()))
        .map_err(|error| format!("cannot tell the user of uwe: {error}"))?;
    let wr = Wr::connect(&config.settings, &home, uid)?;
    let folder = scratch.join("wr");
    crate::grid::executor(wr, &config.jobs, store, &folder, run, default_image, setup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use config_file::ConfigFile;

    fn read(text: &str) -> Result<Config, ConfigError> {
        let config = ConfigFile::parse(text, Path::new("uwe.toml"))?;
        let section = config.section("executor")?.expect("a section");
        Config::read(&section.table("wr")?.expect("a table"))
    }

    #[test]
    fn reads_wr_config() {
        let text = "[executor.wr]\ndeployment = 'development'\ncontainer = 'podman'\n\
                    staging = 'object-store'\nstaging_root = 's3://bucket/uwe'\n\
                    helper_path = '/opt/uwe'\nnode_folder = '/scratch/uwe'\n";
        let config = read(text).expect("a config");
        assert_eq!(config.settings.deployment, "development");
        assert_eq!(config.jobs.container.as_deref(), Some("podman"));
        assert_eq!(config.jobs.node_folder.as_deref(), Some("/scratch/uwe"));
        assert_eq!(config.jobs.helper_file, None);
        assert!(read("[executor.wr]\noptions = ['-q normal']\n").is_err());
        let text = "[executor.wr]\nstaging = 'object-store'\nstaging_root = 's3://b/p'\n\
                    staging_access = 'credentials'\n";
        assert!(read(text).is_err());
    }
}
