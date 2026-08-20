//! The config file of iunctus (decision 20): the site-wide file, the user's file and the file
//! of `-iunctus-config`, read once per command, with the storage providers and the store that
//! they name. Without any file, everything works with its defaults.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;

use config_file::{ConfigError, ConfigFile, Secret, Table};
use content_store::Store;
use slurm_executor::PullResources;
use storage::{Storage, StoreLocation};
use task_runner::NetworkRule;

use crate::Failure;
use crate::arguments::Arguments;
use crate::insight::telemetry::Telemetry;

/// The sections of the config file. Another name is an error, so that a typing mistake does
/// not go unnoticed. `executor` holds the settings of executors.
const SECTIONS: &[&str] = &["store", "storage", "executor", "telemetry", "network"];

/// What the config files set up for a command; the default is what no config file sets up.
#[derive(Default)]
pub struct Setup {
    /// The storage providers, one client each for the whole process.
    pub storage: Storage,
    /// The settings of the executors.
    pub executors: Executors,
    /// Where the traces of runs go, from `[telemetry]`.
    pub telemetry: Option<Telemetry>,
    /// Which tasks get the network, from `[network]`.
    pub network: NetworkRule,
}

/// The settings of executors from the `[executor.<name>]` sections. Options on the command line
/// and environment variables win over them.
#[derive(Clone, Debug, Default)]
pub struct Executors {
    /// `[executor.local] engine`: the container engine, as `IUNCTUS_ENGINE` names it.
    pub engine: Option<String>,
    /// `[executor.tes] server`, like `-tes-server`.
    pub tes_server: Option<String>,
    /// `[executor.tes] token`, like `IUNCTUS_TES_TOKEN`.
    pub tes_token: Option<Secret>,
    /// `[executor.tes.sites.<name>]`: more TES servers, each near some data.
    pub tes_sites: Vec<TesSite>,
    /// `[executor.slurm] container`, like `-slurm-container`.
    pub slurm_container: Option<String>,
    /// `[executor.slurm] options`, a list of options of `sbatch`, like `-slurm-options`.
    pub slurm_options: Option<Vec<String>>,
    /// `[executor.slurm.pull]`: `cpus`, `memory_mb` and `minutes` of the job that pulls an
    /// image into a SIF file.
    pub slurm_pull: PullResources,
    /// `[executor.kubernetes] kubeconfig`, like `-kubeconfig`.
    pub kubeconfig: Option<PathBuf>,
    /// `[executor.kubernetes] namespace`, like `-kubernetes-namespace`.
    pub kubernetes_namespace: Option<String>,
}

/// A site of `[executor.tes.sites.<name>]`: a TES server, and the address prefixes or folders
/// of the data near it, which decide the tasks that run there.
#[derive(Clone, Debug)]
pub struct TesSite {
    pub name: String,
    pub server: String,
    pub token: Option<Secret>,
    pub near: Vec<String>,
}

impl Executors {
    /// The settings of the `[executor]` section of a config file.
    fn read(config: &ConfigFile) -> Result<Executors, ConfigError> {
        let mut settings = Executors::default();
        let Some(section) = config.section("executor")? else {
            return Ok(settings);
        };
        section.only(&["local", "tes", "slurm", "kubernetes"])?;
        if let Some(local) = section.table("local")? {
            local.only(&["engine"])?;
            settings.engine = local.text("engine")?;
        }
        if let Some(tes) = section.table("tes")? {
            tes.only(&["server", "token", "sites"])?;
            settings.tes_server = tes.text("server")?;
            settings.tes_token = tes.secret("token")?;
            for site in (tes.table("sites")?.map(|sites| sites.tables()))
                .transpose()?
                .into_iter()
                .flatten()
            {
                site.only(&["server", "token", "near"])?;
                let server = site.text("server")?;
                let server = server
                    .ok_or_else(|| site.missing("server", "the address of its TES server"))?;
                settings.tes_sites.push(TesSite {
                    // The table is named by its whole path, like `executor.tes.sites.hamburg`.
                    name: (site.name().strip_prefix("executor.tes.sites."))
                        .unwrap_or(site.name())
                        .to_owned(),
                    server,
                    token: site.secret("token")?,
                    near: site.texts("near")?.unwrap_or_default(),
                });
            }
        }
        if let Some(slurm) = section.table("slurm")? {
            slurm.only(&["container", "options", "pull"])?;
            settings.slurm_container = slurm.text("container")?;
            settings.slurm_options = slurm.texts("options")?;
            if let Some(pull) = slurm.table("pull")? {
                settings.slurm_pull = pull_resources(&pull)?;
            }
        }
        if let Some(kubernetes) = section.table("kubernetes")? {
            kubernetes.only(&["kubeconfig", "namespace"])?;
            settings.kubeconfig = kubernetes.text("kubeconfig")?.map(PathBuf::from);
            settings.kubernetes_namespace = kubernetes.text("namespace")?;
        }
        Ok(settings)
    }
}

/// The resources of the job that pulls an image, from `[executor.slurm.pull]`; what it leaves
/// out keeps its default.
fn pull_resources(pull: &Table<'_>) -> Result<PullResources, ConfigError> {
    pull.only(&["cpus", "memory_mb", "minutes"])?;
    let positive = |key: &str| -> Result<Option<u64>, ConfigError> {
        match pull.integer(key)? {
            None => Ok(None),
            Some(value) if value > 0 => Ok(Some(value as u64)),
            Some(value) => Err(pull.invalid(key, format!("is {value}; use a number above 0"))),
        }
    };
    let mut resources = PullResources::default();
    if let Some(cpus) = positive("cpus")? {
        resources.cpus = u32::try_from(cpus).unwrap_or(u32::MAX);
    }
    if let Some(memory_mb) = positive("memory_mb")? {
        resources.memory_mb = memory_mb;
    }
    if let Some(minutes) = positive("minutes")? {
        resources.time_seconds = minutes.saturating_mul(60);
    }
    Ok(resources)
}

/// Which tasks get the network, from `[network]`: `default = "none"` runs the tasks whose
/// workflow does not say without network (`"allowed"` is the default), and `allowed_steps`
/// names the steps that get it in any case.
fn network(config: &ConfigFile) -> Result<NetworkRule, ConfigError> {
    let Some(section) = config.section("network")? else {
        return Ok(NetworkRule::default());
    };
    section.only(&["default", "allowed_steps"])?;
    let none_by_default = match section.text("default")?.as_deref() {
        None | Some("allowed") => false,
        Some("none") => true,
        Some(other) => {
            let message = format!("is `{other}`; use \"none\" or \"allowed\"");
            return Err(section.invalid("default", message));
        }
    };
    Ok(NetworkRule {
        none_by_default,
        allowed_steps: section.texts("allowed_steps")?.unwrap_or_default(),
    })
}

impl Setup {
    /// Reads the config files.
    pub fn load(arguments: &Arguments) -> Result<Setup, Failure> {
        let failed = |error: ConfigError| Failure::setup(format!("iunctus: {error}"));
        let config = ConfigFile::load(arguments.config_file.as_deref()).map_err(failed)?;
        config.root().only(SECTIONS).map_err(failed)?;
        let storage = Storage::from_config(&config).map_err(failed)?;
        let executors = Executors::read(&config).map_err(failed)?;
        let telemetry = Telemetry::read(&config).map_err(failed)?;
        let network = network(&config).map_err(failed)?;
        Ok(Setup {
            storage,
            executors,
            telemetry,
            network,
        })
    }

    /// The container engine as `IUNCTUS_ENGINE` names it, else as `[executor.local] engine`
    /// does; empty for Docker, else Podman.
    pub fn engine(&self) -> String {
        (std::env::var("IUNCTUS_ENGINE")
            .ok()
            .filter(|name| !name.is_empty()))
        .or_else(|| self.executors.engine.clone())
        .unwrap_or_default()
    }

    /// The kind of the container engine that runs the tasks, as Nextflow names it in
    /// `workflow.containerEngine` and in its config scopes, like `docker.registry`: the
    /// runtime of the SLURM jobs with `-executor slurm`, like `apptainer`, so that workflows
    /// choose their images for it as they do in Nextflow; else `docker` or `podman` of this
    /// machine, where a socket counts as Podman when its path says so.
    pub fn engine_kind(&self, arguments: &Arguments) -> String {
        engine_kind(
            arguments.executor.as_deref(),
            self.slurm_container(arguments),
            &self.engine(),
        )
    }

    /// The container runtime of SLURM jobs: `-slurm-container`, else `container` of
    /// `[executor.slurm]`, else `apptainer`.
    pub fn slurm_container<'a>(&'a self, arguments: &'a Arguments) -> &'a str {
        (arguments.slurm_container.as_deref())
            .or(self.executors.slurm_container.as_deref())
            .unwrap_or("apptainer")
    }

    /// Where the cache store lives: `-store`, else `location` of `[store]`, else `store` in
    /// the cache directory.
    pub fn store_location(&self, arguments: &Arguments) -> String {
        (arguments.store.clone())
            .or_else(|| self.storage.store_location().map(str::to_owned))
            .unwrap_or_else(|| {
                let folder = crate::cache_directory().join("store");
                folder.to_string_lossy().into_owned()
            })
    }

    /// The cache store: a folder, or a place of a storage provider that can write there. With
    /// `local` of `[store]`, a folder of this machine stands in front of a store elsewhere:
    /// reads look there first, and new results go to both.
    pub fn store(&self, arguments: &Arguments) -> Result<Store, Failure> {
        let location = self.store_location(arguments);
        let failed = |message: String| Failure::setup(format!("iunctus: {message}"));
        let place = (self.storage.store(&location)).map_err(|error| failed(error.to_string()))?;
        let store = match place {
            StoreLocation::Folder(folder) => Store::local(&folder),
            StoreLocation::Objects(operator) => Store::remote(operator, &location),
        };
        let store = store
            .map_err(|error| failed(format!("cannot open the store at {location}: {error}")))?;
        let Some(local) = self.storage.local_store() else {
            return Ok(store);
        };
        let opened = Store::local(std::path::Path::new(local))
            .and_then(|front| Store::layered(front, store));
        opened.map_err(|error| {
            failed(format!(
                "cannot open the folder {local} in front of the store: {error}; name another with \
                 `local` in [store]"
            ))
        })
    }
}

/// The kind of the engine that runs the tasks of `executor`: the SLURM runtime `slurm`, or the
/// engine of this machine, `local`.
fn engine_kind(executor: Option<&str>, slurm: &str, local: &str) -> String {
    match executor {
        Some("slurm") => slurm.to_owned(),
        _ if local.contains("podman") => "podman".to_owned(),
        _ => "docker".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use config_file::ConfigFile;
    use slurm_executor::PullResources;

    use super::Executors;
    use task_runner::NetworkRule;

    #[test]
    fn nextflow_sees_the_engine_that_runs_the_tasks() {
        let kind = super::engine_kind;
        assert_eq!(kind(Some("slurm"), "apptainer", "docker"), "apptainer");
        assert_eq!(kind(Some("slurm"), "singularity", ""), "singularity");
        assert_eq!(kind(Some("local"), "apptainer", ""), "docker");
        assert_eq!(
            kind(None, "apptainer", "/run/user/1/podman/podman.sock"),
            "podman"
        );
    }

    fn read(text: &str) -> Result<Executors, String> {
        let config =
            ConfigFile::parse(text, Path::new("config.toml")).map_err(|e| e.to_string())?;
        Executors::read(&config).map_err(|error| error.to_string())
    }

    #[test]
    fn executors_take_their_settings_from_the_config_file() {
        let text = "[executor.local]\nengine = \"podman\"\n\n[executor.tes]\nserver = \
                    \"https://tes.example.org\"\ntoken = \"t\"\n\n[executor.slurm]\ncontainer = \
                    \"podman\"\noptions = [\"--account=lab\", \"--qos=low\"]\n\n\
                    [executor.kubernetes]\nkubeconfig = \"/k/kind.yaml\"\nnamespace = \"x\"\n";
        let settings = read(text).expect("settings");
        assert_eq!(settings.engine.as_deref(), Some("podman"));
        assert_eq!(
            settings.tes_server.as_deref(),
            Some("https://tes.example.org")
        );
        let token = settings
            .tes_token
            .expect("a token")
            .reveal()
            .expect("its text");
        assert_eq!(token, "t");
        assert_eq!(settings.slurm_container.as_deref(), Some("podman"));
        let options = settings.slurm_options.expect("options");
        assert_eq!(options, ["--account=lab", "--qos=low"]);
        assert_eq!(settings.slurm_pull, PullResources::default());
        assert_eq!(
            settings.kubeconfig.as_deref(),
            Some(Path::new("/k/kind.yaml"))
        );
        assert_eq!(settings.kubernetes_namespace.as_deref(), Some("x"));
        assert!(settings.tes_sites.is_empty());
        assert!(read("").expect("no settings").engine.is_none());
    }

    #[test]
    fn the_pull_job_of_slurm_asks_for_what_the_config_file_says() {
        let text = "[executor.slurm.pull]\ncpus = 8\nminutes = 120\n";
        let pull = read(text).expect("settings").slurm_pull;
        let expected = PullResources {
            cpus: 8,
            time_seconds: 7200,
            ..PullResources::default()
        };
        assert_eq!(pull, expected);
        let error = read("[executor.slurm.pull]\nmemory_mb = 0\n").expect_err("an error");
        assert!(error.contains("use a number above 0"), "{error}");
        let error = read("[executor.slurm.pull]\nmemory = 8\n").expect_err("an error");
        assert!(error.contains("memory"), "{error}");
    }

    #[test]
    fn the_network_section_names_the_default_and_the_allowed_steps() {
        let parse = |text: &str| -> Result<NetworkRule, String> {
            let config = ConfigFile::parse(text, Path::new("config.toml"))
                .map_err(|error| error.to_string())?;
            super::network(&config).map_err(|error| error.to_string())
        };
        let rule = parse("[network]\ndefault = \"none\"\nallowed_steps = [\"DOWNLOAD_*\"]\n")
            .expect("a rule");
        assert!(rule.none_by_default);
        assert_eq!(rule.allowed_steps, ["DOWNLOAD_*"]);
        assert_eq!(parse("").expect("no rule"), NetworkRule::default());
        let error = parse("[network]\ndefault = \"off\"\n").expect_err("an error");
        assert!(error.contains(":2:") && error.contains("off"), "{error}");
    }

    #[test]
    fn tes_sites_name_their_server_and_the_data_near_them() {
        let text = "[executor.tes]\nserver = \"https://tes.example.org\"\n\n\
                    [executor.tes.sites.hamburg]\nserver = \"https://tes.hh.example.org\"\n\
                    near = [\"s3://hh/\", \"/mnt/hh/\"]\n\n\
                    [executor.tes.sites.heidelberg]\nserver = \"https://tes.hd.example.org\"\n";
        let sites = read(text).expect("settings").tes_sites;
        let names: Vec<&str> = sites.iter().map(|site| site.name.as_str()).collect();
        assert_eq!(names, ["hamburg", "heidelberg"]);
        assert_eq!(sites[0].server, "https://tes.hh.example.org");
        assert_eq!(sites[0].near, ["s3://hh/", "/mnt/hh/"]);
        assert!(sites[1].near.is_empty() && sites[1].token.is_none());
        let error = read("[executor.tes.sites.x]\nnear = [\"/a/\"]\n").expect_err("an error");
        assert!(error.contains("server"), "{error}");
    }

    #[test]
    fn unknown_executors_and_settings_are_errors() {
        let error = read("[executor.tes]\nsrever = \"x\"\n").expect_err("an error");
        assert!(error.contains("srever"), "{error}");
        let error = read("[executor.cloud]\nx = 1\n").expect_err("an error");
        assert!(error.contains("cloud"), "{error}");
    }
}
