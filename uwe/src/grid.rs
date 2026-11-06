//! Builds the HTCondor and PBS executors from `[executor.htcondor]` and `[executor.pbs]`, with
//! shared-path or object-store staging.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use config_file::{ConfigError, Table};
use content_store::Store;
use htcondor_executor::Condor;
use pbs_executor::Pbs;
use slurm_executor::grid::{GridBackend, Scheduler};
use slurm_executor::{Helper, NodeStaging};
use task_runner::{Runner, RunnerExecutor};

/// The settings of `[executor.htcondor]` or `[executor.pbs]`.
#[derive(Clone, Debug, Default)]
pub struct Config {
    /// `container`: how the nodes run containers, `apptainer` by default.
    pub container: Option<String>,
    /// `options`: more lines of the submit description, or more options of `qsub`.
    pub options: Vec<String>,
    pub staging: crate::staging::Settings,
    /// `helper_path`, or `helper_url` with `helper_sha256`: where the jobs get the uwe binary.
    pub helper: Option<Helper>,
    /// Without `helper`: `helper_file`, or else the running binary, which uwe uploads.
    pub helper_file: Option<PathBuf>,
    /// `node_folder`: the folder of the nodes for the files of a job.
    pub node_folder: Option<String>,
    /// For PBS, `project`, `storage` and `disk_resource`, as NCI Gadi needs them.
    pub project: Option<String>,
    pub storage: Option<String>,
    pub disk_resource: Option<String>,
}

impl Config {
    /// The settings of a table; `pbs` allows the settings of PBS jobs.
    pub fn read(table: &Table<'_>, pbs: bool) -> Result<Config, ConfigError> {
        let mut keys = vec!["container", "options", "node_folder", "helper_file"];
        keys.extend(["helper_path", "helper_url", "helper_sha256"]);
        keys.extend(crate::staging::Settings::KEYS);
        if pbs {
            keys.extend(["project", "storage", "disk_resource"]);
        }
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
        Ok(Config {
            container: table.text("container")?,
            options: table.texts("options")?.unwrap_or_default(),
            staging,
            helper,
            helper_file,
            node_folder: table.text("node_folder")?,
            project: table.text("project")?,
            storage: table.text("storage")?,
            disk_resource: table.text("disk_resource")?,
        })
    }
}

/// The executor of `-executor htcondor`, with its attempts in `<scratch>/htcondor`.
pub fn htcondor(
    store: Store,
    scratch: &Path,
    run: &str,
    default_image: Option<String>,
    setup: &crate::setup::Setup,
) -> Result<RunnerExecutor<crate::conda_backend::Remote<GridBackend<Condor>>>, String> {
    let config = &setup.executors.htcondor;
    let condor = Condor {
        commands: None,
        lines: config.options.clone(),
    };
    // `condor_history` only tells how a job ended that left the queue.
    let missing = condor.missing_tools();
    let missing: Vec<&str> = (missing.into_iter())
        .filter(|tool| *tool != "condor_history")
        .collect();
    needs(&missing, "htcondor", "an HTCondor access point")?;
    let folder = scratch.join("htcondor");
    executor(condor, config, store, &folder, run, default_image, setup)
}

/// The executor of `-executor pbs`, with its attempts in `<scratch>/pbs`.
pub fn pbs(
    store: Store,
    scratch: &Path,
    run: &str,
    default_image: Option<String>,
    setup: &crate::setup::Setup,
) -> Result<RunnerExecutor<crate::conda_backend::Remote<GridBackend<Pbs>>>, String> {
    let config = &setup.executors.pbs;
    let pbs = Pbs {
        commands: None,
        options: (config.options.iter())
            .flat_map(|option| option.split_whitespace().map(str::to_owned))
            .collect(),
        project: config.project.clone(),
        storage: config.storage.clone(),
        disk_resource: config.disk_resource.clone(),
    };
    needs(
        &pbs.missing_tools(),
        "pbs",
        "a login node of the PBS cluster",
    )?;
    let folder = scratch.join("pbs");
    executor(pbs, config, store, &folder, run, default_image, setup)
}

/// Refuses a run without the tools of its scheduler.
fn needs(missing: &[&str], executor: &str, place: &str) -> Result<(), String> {
    match missing {
        [] => Ok(()),
        tools => Err(format!(
            "-executor {executor} needs `{}` on the PATH; run uwe on {place}",
            tools.join("`, `")
        )),
    }
}

/// An executor that submits each task as a job of `scheduler`, with its attempts in `folder`.
/// Tasks without an image get `default_image`.
fn executor<S: Scheduler + std::fmt::Debug>(
    scheduler: S,
    config: &Config,
    store: Store,
    folder: &Path,
    run: &str,
    default_image: Option<String>,
    setup: &crate::setup::Setup,
) -> Result<RunnerExecutor<crate::conda_backend::Remote<GridBackend<S>>>, String> {
    let name = config.container.as_deref().unwrap_or("apptainer");
    let container = crate::slurm::container(Some(name)).map_err(|_| {
        format!(
            "unknown container runtime `{name}` for {}; use apptainer, singularity, podman or \
             docker",
            S::NAME
        )
    })?;
    let mut backend = GridBackend::new(scheduler, folder.to_owned(), container);
    backend.proxy = storage::config_proxy().map(str::to_owned);
    backend.staging_time = config.staging.margin();
    if let Some(stage) = config.staging.stage(&setup.storage)? {
        let helper = match (&config.helper, &config.helper_file) {
            (Some(helper), _) => helper.clone(),
            (None, Some(file)) => Helper::Upload(
                executor::binary::Binary::open(file)
                    .map_err(|error| format!("{error}, or set helper_path or helper_url"))?,
            ),
            (None, None) => return Err(format!("{} staging needs helper_path", S::NAME)),
        };
        backend.remote = Some(NodeStaging {
            stage,
            helper,
            folder: config.node_folder.clone(),
            secret_file: None,
        });
    }
    // The nodes read the inputs from the store, so they are copied there.
    let backend = crate::conda_backend::Remote::new(backend, store.clone(), folder, run, setup);
    let settings = crate::runner_settings(setup.storage.clone(), true);
    let mut runner = Runner::new(store, backend, run, settings);
    runner.references(setup.references.clone());
    Ok(RunnerExecutor::new(runner, default_image))
}

#[cfg(test)]
mod tests {
    use super::*;
    use config_file::ConfigFile;

    fn read(text: &str, name: &str) -> Result<Config, ConfigError> {
        let config = ConfigFile::parse(text, Path::new("uwe.toml"))?;
        let section = config.section("executor")?.expect("a section");
        Config::read(&section.table(name)?.expect("a table"), name == "pbs")
    }

    #[test]
    fn reads_scheduler_config() {
        let text = "[executor.pbs]\ncontainer = 'singularity'\noptions = ['-A lab']\n\
                    project = 'ab12'\nstorage = 'gdata/ab12'\ndisk_resource = 'jobfs'\n";
        let config = read(text, "pbs").expect("a config");
        assert_eq!(config.project.as_deref(), Some("ab12"));
        assert_eq!(config.disk_resource.as_deref(), Some("jobfs"));
        assert_eq!(config.options, ["-A lab"]);
        assert!(read("[executor.htcondor]\nproject = 'ab12'\n", "htcondor").is_err());
        let text = "[executor.htcondor]\nstaging = 'object-store'\nstaging_root = 's3://b/p'\n\
                    staging_access = 'credentials'\n";
        assert!(read(text, "htcondor").is_err());
        let text = "[executor.htcondor]\nstaging = 'object-store'\nstaging_root = 's3://b/p'\n\
                    helper_path = '/opt/uwe'\n";
        let config = read(text, "htcondor").expect("a config");
        assert!(matches!(config.helper, Some(Helper::Path(_))));
        assert_eq!(config.helper_file, None);
    }
}
