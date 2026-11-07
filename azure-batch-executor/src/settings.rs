//! Reads the settings of the Azure Batch executor from `[executor.azure-batch]` and its pools from
//! `[executor.azure-batch.pools.<name>]`: account, credentials, pools, cleanup and the helper.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use config_file::{ConfigError, Secret, Table};
use executor::batch::{Helper, TASK_FOLDER, helper};

/// The version of the Azure Batch REST API that every request names.
pub const API_VERSION: &str = "2024-07-01.20.0";

/// The pool settings of auto pools, as Nextflow's `azure.batch.pools.auto`.
pub const AUTO: &str = "auto";

/// How often Azure Batch is asked about a task, by default.
const POLL: Duration = Duration::from_secs(10);

/// The settings of the executor. The Nextflow `azure` scope fills those that the config file of
/// uwe leaves unset.
#[derive(Clone, Debug)]
pub struct Settings {
    /// The Batch account; else `AZURE_BATCH_ACCOUNT_NAME` or the name in the endpoint.
    pub account: Option<String>,
    /// The Shared Key of the account; else `AZURE_BATCH_ACCOUNT_KEY`.
    pub account_key: Option<Hidden>,
    /// The address of the account, like `https://lab.westeurope.batch.azure.com`.
    pub endpoint: Option<String>,
    /// The region of the account, which an endpoint of the usual form names too.
    pub location: Option<String>,
    /// The pool of tasks that name none in their queue.
    pub pool: Option<String>,
    /// Whether tasks without a pool run on a pool that uwe makes from the `auto` pool settings.
    pub auto_pools: bool,
    /// Whether uwe creates named pools that do not exist, from their settings.
    pub allow_pool_creation: bool,
    pub delete_jobs: bool,
    /// Whether uwe deletes the pools that it created at the end of the run.
    pub delete_pools: bool,
    /// Which tasks uwe deletes when they ended: all, none, or `None` for the successful ones.
    pub delete_tasks: Option<bool>,
    /// Whether the jobs end once their tasks ended, at the end of the run.
    pub terminate_jobs: bool,
    pub job_time: Duration,
    /// How often uwe waits for a free job when the account has its most active jobs.
    pub quota_retries: u32,
    pub quota_wait: Duration,
    pub pools: BTreeMap<String, PoolSettings>,
    /// Entra ID: a service principal with a client secret, or a managed identity.
    pub tenant_id: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<Hidden>,
    pub managed_identity: Option<Identity>,
    /// The addresses of the Entra ID token service and of the instance metadata service.
    pub authority_endpoint: String,
    pub identity_endpoint: String,
    pub api_version: String,
    /// The registry that the pools that uwe creates log in to.
    pub registry: Option<Registry>,
    pub poll: Duration,
    /// The folder of the container that holds the folders of attempts.
    pub task_folder: String,
    /// Where the tasks get the static uwe binary; `None` for a binary that uwe uploads.
    pub helper: Option<Helper>,
}

/// A secret value, from the config file of uwe or from the Nextflow config, which `Debug`
/// never shows.
#[derive(Clone, PartialEq, Eq)]
pub enum Hidden {
    Secret(Secret),
    Text(String),
}

/// A managed identity of the machine that runs uwe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Identity {
    System,
    /// A user-assigned identity, by its client id.
    User(String),
}

/// A container registry and its login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registry {
    pub server: String,
    pub user: String,
    pub password: Hidden,
}

/// How uwe creates a pool that does not exist, and what it knows of its VM size.
#[derive(Clone, Debug, PartialEq)]
pub struct PoolSettings {
    pub vm_size: String,
    /// The cores of the VM size, which are the task slots of a node.
    pub vm_cpus: Option<u32>,
    pub vm_memory_gb: Option<f64>,
    pub vm_count: u32,
    pub max_vm_count: Option<u32>,
    pub auto_scale: bool,
    /// An auto scale formula instead of uwe's own; it may name `{{vmCount}}`, `{{maxVmCount}}`,
    /// `{{scaleInterval}}` and `{{poolCreationTime}}`.
    pub scale_formula: Option<String>,
    pub scale_interval: Duration,
    /// Whether the nodes are low priority (spot) VMs.
    pub low_priority: bool,
    /// `spread` or `pack`.
    pub schedule_policy: Option<String>,
    /// A user account of the pool that tasks run as, instead of a task user without rights.
    pub run_as: Option<String>,
    pub publisher: String,
    pub offer: String,
    pub sku: String,
    pub image_id: Option<String>,
    pub unverified_images: bool,
    /// The subnet of the nodes.
    pub virtual_network: Option<String>,
    /// A shell script that each node runs once, as a user without rights.
    pub start_script: Option<String>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            account: None,
            account_key: None,
            endpoint: None,
            location: None,
            pool: None,
            auto_pools: false,
            allow_pool_creation: false,
            delete_jobs: false,
            delete_pools: false,
            delete_tasks: None,
            terminate_jobs: true,
            job_time: Duration::from_secs(30 * 24 * 3600),
            quota_retries: 3,
            quota_wait: Duration::from_secs(120),
            pools: BTreeMap::new(),
            tenant_id: None,
            client_id: None,
            client_secret: None,
            managed_identity: None,
            authority_endpoint: "https://login.microsoftonline.com".into(),
            identity_endpoint: "http://169.254.169.254/metadata/identity/oauth2/token".into(),
            api_version: API_VERSION.into(),
            registry: None,
            poll: POLL,
            task_folder: TASK_FOLDER.into(),
            helper: None,
        }
    }
}

impl Default for PoolSettings {
    fn default() -> PoolSettings {
        PoolSettings {
            vm_size: "Standard_D4a_v4".into(),
            vm_cpus: None,
            vm_memory_gb: None,
            vm_count: 1,
            max_vm_count: None,
            auto_scale: false,
            scale_formula: None,
            scale_interval: Duration::from_secs(5 * 60),
            low_priority: false,
            schedule_policy: None,
            run_as: None,
            publisher: "microsoft-dsvm".into(),
            offer: "ubuntu-hpc".into(),
            sku: "batch.node.ubuntu 24.04".into(),
            image_id: None,
            unverified_images: false,
            virtual_network: None,
            start_script: None,
        }
    }
}

impl fmt::Debug for Hidden {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Hidden(..)")
    }
}

impl Hidden {
    /// The secret itself; a secret of an environment variable must be set.
    pub fn reveal(&self) -> Result<String, String> {
        match self {
            Hidden::Secret(secret) => secret.reveal().map_err(|error| error.to_string()),
            Hidden::Text(text) => Ok(text.clone()),
        }
    }
}

impl Settings {
    pub const KEYS: &'static [&'static str] = &[
        "account",
        "account_key",
        "endpoint",
        "location",
        "pool",
        "auto_pools",
        "allow_pool_creation",
        "delete_jobs",
        "delete_pools",
        "delete_tasks",
        "terminate_jobs",
        "job_max_hours",
        "job_quota_retries",
        "job_quota_seconds",
        "pools",
        "tenant_id",
        "client_id",
        "client_secret",
        "managed_identity",
        "authority_endpoint",
        "identity_endpoint",
        "api_version",
        "registry_server",
        "registry_user",
        "registry_password",
        "poll_seconds",
        "task_folder",
        "helper_path",
        "helper_url",
        "helper_sha256",
    ];

    /// The settings of `[executor.azure-batch]`.
    pub fn read(table: &Table<'_>) -> Result<Settings, ConfigError> {
        let flag = |key: &str| table.flag(key);
        let hidden = |key: &str| Ok::<_, ConfigError>(table.secret(key)?.map(Hidden::Secret));
        let mut settings = Settings {
            account: table.text("account")?,
            account_key: hidden("account_key")?,
            location: table.text("location")?,
            pool: table.text("pool")?,
            auto_pools: flag("auto_pools")?.unwrap_or(false),
            allow_pool_creation: flag("allow_pool_creation")?.unwrap_or(false),
            delete_jobs: flag("delete_jobs")?.unwrap_or(false),
            delete_pools: flag("delete_pools")?.unwrap_or(false),
            delete_tasks: flag("delete_tasks")?,
            terminate_jobs: flag("terminate_jobs")?.unwrap_or(true),
            tenant_id: table.text("tenant_id")?,
            client_id: table.text("client_id")?,
            client_secret: hidden("client_secret")?,
            managed_identity: table
                .text("managed_identity")?
                .map(|text| match text.as_str() {
                    "system" => Identity::System,
                    _ => Identity::User(text),
                }),
            ..Settings::default()
        };
        for key in ["endpoint", "authority_endpoint", "identity_endpoint"] {
            let Some(address) = table.text(key)? else {
                continue;
            };
            if !address.starts_with("https://") && !address.starts_with("http://") {
                return Err(table.invalid(key, "must be an http:// or https:// address"));
            }
            let address = address.trim_end_matches('/').to_owned();
            match key {
                "endpoint" => settings.endpoint = Some(address),
                "authority_endpoint" => settings.authority_endpoint = address,
                _ => settings.identity_endpoint = address,
            }
        }
        if let Some(version) = table.text("api_version")? {
            settings.api_version = version;
        }
        if let Some(hours) = table.integer("job_max_hours")? {
            let hours = u64::try_from(hours)
                .ok()
                .filter(|hours| (1..=180 * 24).contains(hours));
            let hours = hours
                .ok_or_else(|| table.invalid("job_max_hours", "must be between 1 and 4320"))?;
            settings.job_time = Duration::from_secs(hours * 3600);
        }
        if let Some(count) = table.integer("job_quota_retries")? {
            settings.quota_retries = u32::try_from(count)
                .ok()
                .filter(|count| *count <= 100)
                .ok_or_else(|| table.invalid("job_quota_retries", "must be between 0 and 100"))?;
        }
        if let Some(seconds) = table.integer("job_quota_seconds")? {
            let seconds = u64::try_from(seconds)
                .ok()
                .filter(|seconds| *seconds <= 3600);
            let seconds = seconds
                .ok_or_else(|| table.invalid("job_quota_seconds", "must be between 0 and 3600"))?;
            settings.quota_wait = Duration::from_secs(seconds);
        }
        settings.registry = registry(table)?;
        if let Some(seconds) = table.integer("poll_seconds")? {
            if !(1..=3600).contains(&seconds) {
                return Err(table.invalid("poll_seconds", "must be between 1 and 3600"));
            }
            settings.poll = Duration::from_secs(seconds as u64);
        }
        if let Some(folder) = table.text("task_folder")? {
            let plain = !folder.contains(['\'', '"', '\\', '$', '`']);
            if !folder.starts_with('/') || folder.split('/').any(|part| part == "..") || !plain {
                return Err(table.invalid("task_folder", "must be a plain absolute folder"));
            }
            settings.task_folder = folder;
        }
        settings.helper = helper(table)?;
        if let Some(pools) = table.table("pools")? {
            for pool in pools.tables()? {
                let read = PoolSettings::read(&pool)?;
                // The name of the table is its full path, like `executor.azure-batch.pools.p`.
                let name = pool.name().rsplit('.').next().unwrap_or_default();
                settings.pools.insert(name.to_owned(), read);
            }
        }
        Ok(settings)
    }

    /// The settings of the pool `id`: its own, else those of all pools that uwe creates.
    pub fn pool_settings(&self, id: &str) -> PoolSettings {
        (self.pools.get(id).cloned()).unwrap_or_default()
    }
}

/// The registry of `registry_server`, `registry_user` and `registry_password`.
fn registry(table: &Table<'_>) -> Result<Option<Registry>, ConfigError> {
    let server = table.text("registry_server")?;
    let user = table.text("registry_user")?;
    let password = table.secret("registry_password")?.map(Hidden::Secret);
    match (user, password) {
        (None, None) if server.is_none() => Ok(None),
        (Some(user), Some(password)) => Ok(Some(Registry {
            server: server.unwrap_or_else(|| "docker.io".into()),
            user,
            password,
        })),
        _ => Err(table.missing(
            "registry_user",
            "a registry login needs registry_user and registry_password",
        )),
    }
}

impl PoolSettings {
    pub const KEYS: &'static [&'static str] = &[
        "vm_size",
        "vm_cpus",
        "vm_memory_gb",
        "vm_count",
        "max_vm_count",
        "auto_scale",
        "scale_formula",
        "scale_minutes",
        "low_priority",
        "schedule_policy",
        "run_as",
        "publisher",
        "offer",
        "sku",
        "vm_image_id",
        "allow_unverified_images",
        "virtual_network",
        "start_script",
        "privileged",
        "start_privileged",
    ];

    /// The settings of `[executor.azure-batch.pools.<name>]`.
    pub fn read(table: &Table<'_>) -> Result<PoolSettings, ConfigError> {
        table.only(PoolSettings::KEYS)?;
        for key in ["privileged", "start_privileged"] {
            if table.flag(key)? == Some(true) {
                return Err(table.invalid(key, PRIVILEGED));
            }
        }
        let defaults = PoolSettings::default();
        let count = |key: &str| -> Result<Option<u32>, ConfigError> {
            let Some(count) = table.integer(key)? else {
                return Ok(None);
            };
            let count = u32::try_from(count).ok().filter(|count| *count <= 10_000);
            Ok(Some(count.ok_or_else(|| {
                table.invalid(key, "must be between 0 and 10000")
            })?))
        };
        let settings = PoolSettings {
            vm_size: table.text("vm_size")?.unwrap_or(defaults.vm_size),
            vm_cpus: count("vm_cpus")?.filter(|cpus| *cpus > 0),
            vm_memory_gb: table.number("vm_memory_gb")?.filter(|gb| *gb > 0.0),
            vm_count: count("vm_count")?.unwrap_or(defaults.vm_count),
            max_vm_count: count("max_vm_count")?,
            auto_scale: table.flag("auto_scale")?.unwrap_or(false),
            scale_formula: table.text("scale_formula")?,
            scale_interval: match count("scale_minutes")? {
                Some(minutes) if minutes > 0 => Duration::from_secs(u64::from(minutes) * 60),
                Some(_) => return Err(table.invalid("scale_minutes", "must be above 0")),
                None => defaults.scale_interval,
            },
            low_priority: table.flag("low_priority")?.unwrap_or(false),
            schedule_policy: table.text("schedule_policy")?,
            run_as: table.text("run_as")?,
            publisher: table.text("publisher")?.unwrap_or(defaults.publisher),
            offer: table.text("offer")?.unwrap_or(defaults.offer),
            sku: table.text("sku")?.unwrap_or(defaults.sku),
            image_id: table.text("vm_image_id")?,
            unverified_images: table.flag("allow_unverified_images")?.unwrap_or(false),
            virtual_network: table.text("virtual_network")?,
            start_script: table.text("start_script")?,
        };
        if !matches!(
            settings.schedule_policy.as_deref(),
            None | Some("spread" | "pack")
        ) {
            return Err(table.invalid("schedule_policy", "must be spread or pack"));
        }
        Ok(settings)
    }
}

/// Why uwe refuses privileged tasks and start tasks.
pub const PRIVILEGED: &str =
    "uwe runs no privileged tasks or start tasks on Azure Batch (least privilege); leave it false";

#[cfg(test)]
mod tests {
    use super::*;
    use config_file::ConfigFile;

    fn read(text: &str) -> Result<Settings, ConfigError> {
        let config = ConfigFile::parse(text, std::path::Path::new("uwe.toml"))?;
        let section = config.section("executor")?.expect("a section");
        Settings::read(&section.table("azure-batch")?.expect("a table"))
    }

    #[test]
    fn reads_batch_settings() {
        let text = "[executor.azure-batch]\naccount = 'lab'\naccount_key = 'a2V5'\n\
                    location = 'westeurope'\npool = 'small'\nallow_pool_creation = true\n\
                    delete_tasks = false\njob_max_hours = 48\nmanaged_identity = 'system'\n\
                    endpoint = 'http://127.0.0.1:1/'\nregistry_user = 'me'\n\
                    registry_password = 'pw'\npoll_seconds = 5\nhelper_path = '/opt/uwe/uwe'\n\
                    [executor.azure-batch.pools.small]\nvm_size = 'Standard_E2d_v5'\n\
                    vm_cpus = 2\nvm_memory_gb = 16\nlow_priority = true\n\
                    schedule_policy = 'pack'\n";
        let settings = read(text).expect("settings");
        assert_eq!(settings.account.as_deref(), Some("lab"));
        let key = settings.account_key.as_ref().map(Hidden::reveal);
        assert_eq!(key, Some(Ok("a2V5".into())));
        assert!(
            !format!("{settings:?}").contains("a2V5"),
            "the key stays hidden"
        );
        assert_eq!(settings.endpoint.as_deref(), Some("http://127.0.0.1:1"));
        assert!(settings.allow_pool_creation && !settings.auto_pools);
        assert_eq!(settings.delete_tasks, Some(false));
        assert_eq!(settings.job_time, Duration::from_secs(48 * 3600));
        assert_eq!(settings.managed_identity, Some(Identity::System));
        let registry = settings.registry.as_ref().expect("a registry");
        assert_eq!(
            (registry.server.as_str(), registry.user.as_str()),
            ("docker.io", "me")
        );
        assert_eq!(settings.helper, Some(Helper::Path("/opt/uwe/uwe".into())));
        let pool = settings.pool_settings("small");
        assert_eq!(
            (pool.vm_size.as_str(), pool.vm_cpus),
            ("Standard_E2d_v5", Some(2))
        );
        assert!(pool.low_priority);
        assert_eq!(settings.pool_settings("other"), PoolSettings::default());
        for wrong in [
            "endpoint = 'lab.batch.azure.com'",
            "job_max_hours = 0",
            "registry_user = 'me'",
            "task_folder = \"/tmp/it's\"",
            "[executor.azure-batch.pools.p]\nprivileged = true",
            "[executor.azure-batch.pools.p]\nstart_privileged = true",
            "[executor.azure-batch.pools.p]\nschedule_policy = 'fill'",
            "[executor.azure-batch.pools.p]\nvm_type = 'x'",
        ] {
            let error = read(&format!("[executor.azure-batch]\n{wrong}\n"));
            assert!(error.is_err(), "{wrong}");
        }
    }
}
