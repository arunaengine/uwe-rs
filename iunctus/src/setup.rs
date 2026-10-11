//! Loads executor settings, command defaults and storage configuration.
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
const SECTIONS: &[&str] = &[
    "store",
    "storage",
    "executor",
    "telemetry",
    "registry",
    "network",
    "cache",
    "report",
    "conda",
    "page",
    "retry",
    "secrets",
];

/// The executors that `-executor` and `default` of `[executor]` may name.
const EXECUTORS: &[&str] = &["simulator", "local", "tes", "slurm", "kubernetes"];

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
    pub conda: Option<crate::conda::Config>,
    pub secrets: std::collections::BTreeMap<String, Secret>,
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
    pub tes: tes_executor::Settings,
    /// `[executor.tes.sites.<name>]`: more TES servers, each near some data.
    pub tes_sites: Vec<TesSite>,
    /// `[executor.slurm] container`, like `-slurm-container`.
    pub slurm_container: Option<String>,
    /// `[executor.slurm] options`, a list of options of `sbatch`, like `-slurm-options`.
    pub slurm_options: Option<Vec<String>>,
    /// `[executor.slurm.pull]`: `cpus`, `memory_mb` and `minutes` of the job that pulls an
    /// image into a SIF file.
    pub slurm_pull: PullResources,
    /// `[executor.slurm] staging` and the settings of its staging root.
    pub slurm_staging: crate::staging::Settings,
    /// `[executor.slurm] helper_path`, or `helper_url` with `helper_sha256`: where the jobs get
    /// the static iunctus binary for object-store staging.
    pub slurm_helper: Option<slurm_executor::Helper>,
    /// `[executor.slurm] node_folder`: the folder of the nodes for the files of a job.
    pub slurm_node_folder: Option<String>,
    /// `[executor.kubernetes] kubeconfig`, like `-kubeconfig`.
    pub kubeconfig: Option<PathBuf>,
    /// `[executor.kubernetes] namespace`, like `-kubernetes-namespace`.
    pub kubernetes_namespace: Option<String>,
    /// `[executor.kubernetes] staging` and the settings of its staging root.
    pub kubernetes_staging: crate::staging::Settings,
    /// `[executor.kubernetes] helper_image` and `helper_mount`: the image that holds only the
    /// static iunctus binary, for object-store staging.
    pub kubernetes_helper: Option<kubernetes_executor::Helper>,
}

/// A site of `[executor.tes.sites.<name>]`: a TES server, and the address prefixes or folders
/// of the data near it, which decide the tasks that run there.
#[derive(Clone, Debug)]
pub struct TesSite {
    pub name: String,
    pub server: String,
    pub token: Option<Secret>,
    pub near: Vec<String>,
    pub settings: tes_executor::Settings,
}

impl Executors {
    /// The settings of the `[executor]` section of a config file.
    fn read(config: &ConfigFile) -> Result<Executors, ConfigError> {
        let mut settings = Executors::default();
        let Some(section) = config.section("executor")? else {
            return Ok(settings);
        };
        section.only(&["default", "local", "tes", "slurm", "kubernetes"])?;
        if let Some(local) = section.table("local")? {
            local.only(&["engine"])?;
            settings.engine = local.text("engine")?;
        }
        if let Some(tes) = section.table("tes")? {
            let mut keys = vec!["server", "token", "sites"];
            keys.extend(tes_executor::Settings::KEYS);
            tes.only(&keys)?;
            settings.tes_server = tes.text("server")?;
            settings.tes_token = tes.secret("token")?;
            settings.tes = tes_executor::Settings::read(&tes)?;
            for site in (tes.table("sites")?.map(|sites| sites.tables()))
                .transpose()?
                .into_iter()
                .flatten()
            {
                let mut keys = vec!["server", "token", "near"];
                keys.extend(tes_executor::Settings::KEYS);
                site.only(&keys)?;
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
                    settings: tes_executor::Settings::read(&site)?,
                });
            }
        }
        if let Some(slurm) = section.table("slurm")? {
            let mut keys = vec!["container", "options", "pull", "node_folder"];
            keys.extend(["helper_path", "helper_url", "helper_sha256"]);
            keys.extend(crate::staging::Settings::KEYS);
            slurm.only(&keys)?;
            settings.slurm_staging = crate::staging::Settings::read(&slurm, &["object-store"])?;
            settings.slurm_helper = helper(&slurm)?;
            settings.slurm_node_folder = slurm.text("node_folder")?;
            if settings.slurm_staging.mode == crate::staging::Mode::ObjectStore
                && settings.slurm_helper.is_none()
            {
                return Err(slurm.missing(
                    "helper_path",
                    "the path of the static iunctus binary on the nodes, or helper_url and \
                     helper_sha256",
                ));
            }
            settings.slurm_container = slurm.text("container")?;
            settings.slurm_options = slurm.texts("options")?;
            if let Some(pull) = slurm.table("pull")? {
                settings.slurm_pull = pull_resources(&pull)?;
            }
        }
        if let Some(kubernetes) = section.table("kubernetes")? {
            let mut keys = vec!["kubeconfig", "namespace", "helper_image", "helper_mount"];
            keys.extend(crate::staging::Settings::KEYS);
            kubernetes.only(&keys)?;
            settings.kubeconfig = kubernetes.text("kubeconfig")?.map(PathBuf::from);
            settings.kubernetes_namespace = kubernetes.text("namespace")?;
            let modes = ["object-store"];
            settings.kubernetes_staging = crate::staging::Settings::read(&kubernetes, &modes)?;
            let image_volume = match kubernetes.text("helper_mount")?.as_deref() {
                None | Some("image-volume") => true,
                Some("init-container") => false,
                Some(_) => {
                    let message = "must be image-volume or init-container";
                    return Err(kubernetes.invalid("helper_mount", message));
                }
            };
            settings.kubernetes_helper =
                (kubernetes.text("helper_image")?).map(|image| kubernetes_executor::Helper {
                    image,
                    image_volume,
                });
            if settings.kubernetes_staging.mode == crate::staging::Mode::ObjectStore
                && settings.kubernetes_helper.is_none()
            {
                return Err(kubernetes.missing(
                    "helper_image",
                    "an image that holds only the static iunctus binary at /iunctus",
                ));
            }
        }
        Ok(settings)
    }
}

/// Where the SLURM jobs get the iunctus binary: `helper_path` on the nodes, or `helper_url` with
/// its `helper_sha256`.
fn helper(slurm: &Table<'_>) -> Result<Option<slurm_executor::Helper>, ConfigError> {
    let (path, url) = (slurm.text("helper_path")?, slurm.text("helper_url")?);
    let sha256 = slurm.text("helper_sha256")?;
    match (path, url, sha256) {
        (None, None, None) => Ok(None),
        (Some(path), None, None) => Ok(Some(slurm_executor::Helper::Path(path))),
        (None, Some(url), Some(sha256))
            if sha256.len() == 64 && sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            let sha256 = sha256.to_ascii_lowercase();
            Ok(Some(slurm_executor::Helper::Download { url, sha256 }))
        }
        (None, Some(_), _) => Err(slurm.invalid(
            "helper_sha256",
            "must be the SHA-256 of the file at helper_url, as 64 hex digits",
        )),
        _ => Err(slurm.invalid(
            "helper_path",
            "takes no helper_url; name the binary on the nodes or the address to download",
        )),
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
    section.only(&["default", "allowed_steps", "proxy"])?;
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

/// Config defaults applied before command options, cache and network setup.
#[derive(Debug, Default, PartialEq)]
pub struct Defaults {
    pub executor: Option<String>,
    pub cache_folder: Option<PathBuf>,
    pub proxy: Option<String>,
    pub report: Option<PathBuf>,
    pub machine_retries: Option<u32>,
    pub retry_delay_ms: Option<u64>,
}

impl Defaults {
    /// Reads the config files.
    pub fn load(arguments: &Arguments) -> Result<Defaults, Failure> {
        let failed = |error: ConfigError| Failure::setup(format!("iunctus: {error}"));
        let config = ConfigFile::load(arguments.config_file.as_deref()).map_err(failed)?;
        Defaults::read(&config).map_err(failed)
    }

    fn read(config: &ConfigFile) -> Result<Defaults, ConfigError> {
        let mut defaults = Defaults::default();
        if let Some(executor) = config.section("executor")? {
            defaults.executor = executor.text("default")?;
            if let Some(name) = &defaults.executor
                && !EXECUTORS.contains(&name.as_str())
            {
                let message = format!("is `{name}`; use one of {}", EXECUTORS.join(", "));
                return Err(executor.invalid("default", message));
            }
        }
        if let Some(cache) = config.section("cache")? {
            cache.only(&["folder"])?;
            defaults.cache_folder = cache.text("folder")?.map(PathBuf::from);
            if let Some(folder) = &defaults.cache_folder
                && !folder.is_absolute()
            {
                let message = format!("is `{}`; use an absolute folder", folder.display());
                return Err(cache.invalid("folder", message));
            }
        }
        if let Some(network) = config.section("network")? {
            defaults.proxy = network.text("proxy")?;
        }
        if let Some(report) = config.section("report")? {
            report.only(&["file"])?;
            defaults.report = report.text("file")?.map(PathBuf::from);
        }
        if let Some(retry) = config.section("retry")? {
            retry.only(&["machine_count", "base_delay_ms"])?;
            defaults.machine_retries = retry
                .integer("machine_count")?
                .map(|number| {
                    u32::try_from(number)
                        .map_err(|_| retry.invalid("machine_count", "use a whole number from zero"))
                })
                .transpose()?;
            defaults.retry_delay_ms = retry
                .integer("base_delay_ms")?
                .map(|number| {
                    u64::try_from(number)
                        .ok()
                        .filter(|n| *n > 0)
                        .ok_or_else(|| retry.invalid("base_delay_ms", "use a number above zero"))
                })
                .transpose()?;
        }
        Ok(defaults)
    }

    /// Applies the defaults to a command: `-executor` wins over the file for `run` and
    /// `check-machine`; other commands, like `iunctus graph`, keep the simulator. `-report`
    /// wins over the file for `run`.
    pub fn apply(self, command: &str, arguments: &mut Arguments) {
        if matches!(
            command,
            "run" | "graph" | "check-order" | "repeat" | "replay"
        ) {
            arguments.machine_retries = arguments.machine_retries.or(self.machine_retries);
            arguments.retry_delay_ms = arguments.retry_delay_ms.or(self.retry_delay_ms);
        }
        if matches!(command, "run" | "check-machine") && arguments.executor.is_none() {
            arguments.executor = self.executor.or_else(|| Some("local".into()));
        } else if matches!(
            command,
            "graph" | "check" | "check-order" | "inputs" | "outputs"
        ) && arguments.executor.is_none()
        {
            arguments.executor = Some("simulator".into());
        }
        if command == "run" && arguments.report.is_none() {
            arguments.report = self.report;
        }
        if let Some(folder) = self.cache_folder {
            crate::use_cache_folder(folder);
        }
        if let Some(proxy) = self.proxy {
            storage::use_proxy(&proxy);
        }
    }
}

impl Setup {
    /// Reads the config files.
    pub fn load(arguments: &Arguments) -> Result<Setup, Failure> {
        let failed = |error: ConfigError| Failure::setup(format!("iunctus: {error}"));
        let config = ConfigFile::load(arguments.config_file.as_deref()).map_err(failed)?;
        config.root().only(SECTIONS).map_err(failed)?;
        executor::credentials::configure(&config).map_err(failed)?;
        let storage = Storage::from_config(&config).map_err(failed)?;
        let executors = Executors::read(&config).map_err(failed)?;
        let telemetry = Telemetry::read(&config).map_err(failed)?;
        let network = network(&config).map_err(failed)?;
        let conda = crate::conda::Config::read(&config).map_err(failed)?;
        let mut secrets = std::collections::BTreeMap::new();
        if let Some(section) = config.section("secrets").map_err(failed)? {
            for name in section.keys() {
                if let Some(secret) = section.secret(name).map_err(failed)? {
                    if secret.variable().is_none() {
                        return Err(failed(section.invalid(
                            name,
                            "secrets require an environment variable reference",
                        )));
                    }
                    secrets.insert(name.to_owned(), secret);
                }
            }
        }
        Ok(Setup {
            storage,
            executors,
            telemetry,
            network,
            conda,
            secrets,
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

    /// Names the task engine for Nextflow metadata and config scope selection.
    /// Uses the SLURM runtime or this machine's Docker/Podman engine.
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
    fn loads_object_staging() {
        let settings = read("[executor.tes]\nserver = 'https://tes.example.org'\nstaging = 'object-store'\nstaging_root = 's3://bucket/tes'\nidempotency_tag = 'example.org/key'\nuser = { environment = 'TES_USER' }\npassword = { environment = 'TES_PASSWORD' }\nmax_inputs = 200\n").unwrap();
        assert_eq!(
            settings.tes.staging_root.as_deref(),
            Some("s3://bucket/tes")
        );
        assert_eq!(
            settings.tes.idempotency_tag.as_deref(),
            Some("example.org/key")
        );
        assert_eq!(settings.tes.max_inputs, 200);
        assert_eq!(
            settings.tes.user.as_ref().unwrap().variable(),
            Some("TES_USER")
        );
        assert_eq!(
            settings.tes.password.as_ref().unwrap().variable(),
            Some("TES_PASSWORD")
        );
        assert!(read("[executor.tes]\nstaging = 'object-store'\n").is_err());
    }

    #[test]
    fn retry_options_override() {
        let config = ConfigFile::parse(
            "[retry]\nmachine_count = 4\nbase_delay_ms = 250\n",
            Path::new("config.toml"),
        )
        .unwrap();
        let mut arguments = crate::arguments::Arguments {
            machine_retries: Some(0),
            ..Default::default()
        };
        super::Defaults::read(&config)
            .unwrap()
            .apply("run", &mut arguments);
        assert_eq!(arguments.machine_retries, Some(0));
        assert_eq!(arguments.retry_delay_ms, Some(250));
        for setting in ["machine_count = -1", "base_delay_ms = 0"] {
            let config =
                ConfigFile::parse(&format!("[retry]\n{setting}\n"), Path::new("config.toml"))
                    .unwrap();
            assert!(super::Defaults::read(&config).is_err());
        }
    }

    #[test]
    fn exposes_task_engine() {
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
    fn loads_executor_settings() {
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
    fn loads_slurm_resources() {
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
    fn loads_network_policy() {
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
    fn loads_tes_sites() {
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

    fn defaults(text: &str) -> Result<super::Defaults, String> {
        let config =
            ConfigFile::parse(text, Path::new("config.toml")).map_err(|e| e.to_string())?;
        super::Defaults::read(&config).map_err(|error| error.to_string())
    }

    #[test]
    fn loads_command_defaults() {
        let text = "[executor]\ndefault = \"slurm\"\n\n[cache]\nfolder = \"/vol/cache\"\n\n\
                    [network]\nproxy = \"http://proxy.example.org:3128\"\n\n\
                    [report]\nfile = \"report.html\"\n";
        let found = defaults(text).expect("defaults");
        assert_eq!(found.executor.as_deref(), Some("slurm"));
        assert_eq!(found.cache_folder.as_deref(), Some(Path::new("/vol/cache")));
        assert_eq!(
            found.proxy.as_deref(),
            Some("http://proxy.example.org:3128")
        );
        assert_eq!(found.report.as_deref(), Some(Path::new("report.html")));
        assert_eq!(defaults("").expect("none"), super::Defaults::default());
        let error = defaults("[report]\nfolder = \"r\"\n").expect_err("an error");
        assert!(error.contains("folder"), "{error}");
        let error = defaults("[executor]\ndefault = \"cloud\"\n").expect_err("an error");
        assert!(error.contains(":2:") && error.contains("cloud"), "{error}");
        let error = defaults("[cache]\nfolder = \"cache\"\n").expect_err("an error");
        assert!(error.contains("absolute"), "{error}");
        let error = defaults("[cache]\nplace = \"/c\"\n").expect_err("an error");
        assert!(error.contains("place"), "{error}");
        // `default` is no unknown executor for the settings of the executors.
        assert!(read("[executor]\ndefault = \"slurm\"\n").is_ok());
    }

    #[test]
    fn executor_option_overrides() {
        let slurm = || super::Defaults {
            executor: Some("slurm".into()),
            ..super::Defaults::default()
        };
        let mut arguments = crate::arguments::parse(&["main.nf".into()]).expect("arguments");
        slurm().apply("run", &mut arguments);
        assert_eq!(arguments.executor.as_deref(), Some("slurm"));
        let words = ["main.nf".into(), "-executor".into(), "local".into()];
        let mut arguments = crate::arguments::parse(&words).expect("arguments");
        slurm().apply("run", &mut arguments);
        assert_eq!(arguments.executor.as_deref(), Some("local"));
        let mut arguments = crate::arguments::parse(&["main.nf".into()]).expect("arguments");
        slurm().apply("graph", &mut arguments);
        assert_eq!(arguments.executor.as_deref(), Some("simulator"));
        for command in ["run", "check-machine"] {
            let mut arguments = crate::arguments::Arguments::default();
            super::Defaults::default().apply(command, &mut arguments);
            assert_eq!(arguments.executor.as_deref(), Some("local"));
        }
        for command in ["graph", "check", "check-order", "inputs", "outputs"] {
            let mut arguments = crate::arguments::Arguments::default();
            slurm().apply(command, &mut arguments);
            assert_eq!(arguments.executor.as_deref(), Some("simulator"));
        }
    }

    #[test]
    fn rejects_unknown_settings() {
        let error = read("[executor.tes]\nsrever = \"x\"\n").expect_err("an error");
        assert!(error.contains("srever"), "{error}");
        let error = read("[executor.cloud]\nx = 1\n").expect_err("an error");
        assert!(error.contains("cloud"), "{error}");
    }
}
