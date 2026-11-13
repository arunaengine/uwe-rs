//! Reads the settings of the Google Batch executor from `[executor.google-batch]`: project,
//! location, credentials, machines, spot, network, logs, polling and the uwe helper.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;
use std::time::Duration;

use config_file::{ConfigError, Table};
use executor::batch::{Helper, TASK_FOLDER, helper};

/// The Batch location of the jobs when none is set, as Nextflow's `google.location`.
pub const LOCATION: &str = "us-central1";

/// The exit code that Google Batch gives a task whose spot VM went away.
pub const PREEMPTED: i64 = 50001;

/// How often Google Batch is asked about a job, by default, as Nextflow does.
const POLL: Duration = Duration::from_secs(10);

/// The settings of the executor. The Nextflow `google` scope fills those that the config file
/// of uwe leaves unset.
#[derive(Clone, Debug)]
pub struct Settings {
    /// The project of the jobs; else the `project_id` of the credentials file, or
    /// `GOOGLE_CLOUD_PROJECT`.
    pub project: Option<String>,
    /// The Batch location (region) of the jobs; `None` for [`LOCATION`].
    pub location: Option<String>,
    /// A service account key or user credentials file; else `GOOGLE_APPLICATION_CREDENTIALS`,
    /// gcloud's application default credentials, or the metadata server of a Google VM.
    pub credential_file: Option<PathBuf>,
    /// The email of the service account that the VMs of the jobs run as.
    pub service_account: Option<String>,
    /// The machine type of the VMs, or `template://<name>` for an instance template.
    pub machine_type: Option<String>,
    /// The GPU type of tasks that name none, like `nvidia-tesla-t4`.
    pub accelerator_type: Option<String>,
    pub spot: bool,
    /// The older form of spot VMs.
    pub preemptible: bool,
    /// How often Google Batch runs a task again after its spot VM went away; 0 for never.
    pub spot_attempts: u32,
    /// The exit codes on which Google Batch runs a task again, with `spot_attempts`.
    pub retry_codes: Vec<i64>,
    pub boot_disk_image: Option<String>,
    /// The boot disk of tasks without a disk of their own, in GB.
    pub boot_disk_gb: Option<u64>,
    /// The least CPU platform of the VMs, like `Intel Skylake`.
    pub cpu_platform: Option<String>,
    /// Whether Google Batch installs GPU drivers on VMs of an instance template.
    pub gpu_drivers: bool,
    pub ops_agent: bool,
    /// The zones or regions where the VMs may run, like `regions/europe-west4`.
    pub allowed_locations: Vec<String>,
    pub network: Option<String>,
    pub subnetwork: Option<String>,
    /// Whether the VMs get no external IP address.
    pub private_address: bool,
    pub network_tags: Vec<String>,
    /// A `gs://` folder for the logs of the jobs, instead of Cloud Logging.
    pub logs_path: Option<String>,
    pub poll: Duration,
    /// The folder of the VMs that holds the folders of attempts; the containers see it at the
    /// same path.
    pub task_folder: String,
    /// Where the jobs get the static uwe binary; `None` for a binary that uwe uploads.
    pub helper: Option<Helper>,
    /// The image that downloads the helper, which needs `sh`, `wget` and `sha256sum`.
    pub download_image: Option<String>,
    /// The addresses of the Google Batch and Cloud Logging APIs.
    pub batch_endpoint: String,
    pub logging_endpoint: String,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            project: None,
            location: None,
            credential_file: None,
            service_account: None,
            machine_type: None,
            accelerator_type: None,
            spot: false,
            preemptible: false,
            spot_attempts: 0,
            retry_codes: vec![PREEMPTED],
            boot_disk_image: None,
            boot_disk_gb: None,
            cpu_platform: None,
            gpu_drivers: false,
            ops_agent: false,
            allowed_locations: Vec::new(),
            network: None,
            subnetwork: None,
            private_address: false,
            network_tags: Vec::new(),
            logs_path: None,
            poll: POLL,
            task_folder: TASK_FOLDER.into(),
            helper: None,
            download_image: None,
            batch_endpoint: "https://batch.googleapis.com".into(),
            logging_endpoint: "https://logging.googleapis.com".into(),
        }
    }
}

impl Settings {
    pub const KEYS: &'static [&'static str] = &[
        "project",
        "location",
        "credential_file",
        "service_account",
        "machine_type",
        "accelerator_type",
        "spot",
        "preemptible",
        "max_spot_attempts",
        "boot_disk_image",
        "boot_disk_gb",
        "cpu_platform",
        "install_gpu_drivers",
        "install_ops_agent",
        "allowed_locations",
        "network",
        "subnetwork",
        "private_address",
        "network_tags",
        "logs_path",
        "poll_seconds",
        "task_folder",
        "helper_path",
        "helper_url",
        "helper_sha256",
        "helper_download_image",
        "batch_endpoint",
        "logging_endpoint",
    ];

    /// The settings of `[executor.google-batch]`.
    pub fn read(table: &Table<'_>) -> Result<Settings, ConfigError> {
        let flag = |key: &str| Ok::<bool, ConfigError>(table.flag(key)?.unwrap_or(false));
        let mut settings = Settings {
            project: table.text("project")?,
            location: table.text("location")?,
            credential_file: table.text("credential_file")?.map(PathBuf::from),
            service_account: table.text("service_account")?,
            machine_type: table.text("machine_type")?,
            accelerator_type: table.text("accelerator_type")?,
            spot: flag("spot")?,
            preemptible: flag("preemptible")?,
            boot_disk_image: table.text("boot_disk_image")?,
            cpu_platform: table.text("cpu_platform")?,
            gpu_drivers: flag("install_gpu_drivers")?,
            ops_agent: flag("install_ops_agent")?,
            allowed_locations: table.texts("allowed_locations")?.unwrap_or_default(),
            network: table.text("network")?,
            subnetwork: table.text("subnetwork")?,
            private_address: flag("private_address")?,
            network_tags: table.texts("network_tags")?.unwrap_or_default(),
            logs_path: table.text("logs_path")?,
            download_image: table.text("helper_download_image")?,
            ..Settings::default()
        };
        if settings.spot && settings.preemptible {
            return Err(table.invalid("preemptible", "takes no spot; choose one of them"));
        }
        if let Some(count) = table.integer("max_spot_attempts")? {
            settings.spot_attempts = u32::try_from(count)
                .ok()
                .filter(|count| *count <= 10)
                .ok_or_else(|| table.invalid("max_spot_attempts", "must be between 0 and 10"))?;
        }
        if let Some(size) = table.integer("boot_disk_gb")? {
            let size = u64::try_from(size).ok().filter(|size| *size > 0);
            settings.boot_disk_gb =
                Some(size.ok_or_else(|| table.invalid("boot_disk_gb", "must be above 0"))?);
        }
        if (settings.logs_path.as_deref()).is_some_and(|path| !path.starts_with("gs://")) {
            return Err(table.invalid("logs_path", "must be a gs:// folder"));
        }
        if let Some(seconds) = table.integer("poll_seconds")? {
            if !(1..=3600).contains(&seconds) {
                return Err(table.invalid("poll_seconds", "must be between 1 and 3600"));
            }
            settings.poll = Duration::from_secs(seconds as u64);
        }
        if let Some(folder) = table.text("task_folder")? {
            if !folder.starts_with('/') || folder.split('/').any(|part| part == "..") {
                return Err(table.invalid("task_folder", "must be an absolute folder"));
            }
            settings.task_folder = folder;
        }
        for key in ["batch_endpoint", "logging_endpoint"] {
            let Some(address) = table.text(key)? else {
                continue;
            };
            if !address.starts_with("https://") && !address.starts_with("http://") {
                return Err(table.invalid(key, "must be an http:// or https:// address"));
            }
            let address = address.trim_end_matches('/').to_owned();
            match key {
                "batch_endpoint" => settings.batch_endpoint = address,
                _ => settings.logging_endpoint = address,
            }
        }
        settings.helper = helper(table)?;
        Ok(settings)
    }

    /// The Batch location of the jobs.
    pub fn location(&self) -> &str {
        self.location.as_deref().unwrap_or(LOCATION)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use config_file::ConfigFile;

    fn read(text: &str) -> Result<Settings, ConfigError> {
        let config = ConfigFile::parse(text, std::path::Path::new("uwe.toml"))?;
        let section = config.section("executor")?.expect("a section");
        Settings::read(&section.table("google-batch")?.expect("a table"))
    }

    #[test]
    fn reads_batch_settings() {
        let text = "[executor.google-batch]\nproject = 'lab'\nlocation = 'europe-west4'\n\
                    machine_type = 'n2-standard-4'\nspot = true\nmax_spot_attempts = 3\n\
                    boot_disk_gb = 60\nnetwork_tags = ['uwe']\nprivate_address = true\n\
                    poll_seconds = 5\nhelper_path = '/opt/uwe/uwe'\n\
                    batch_endpoint = 'http://127.0.0.1:1/'\n";
        let settings = read(text).expect("settings");
        assert_eq!(settings.project.as_deref(), Some("lab"));
        assert_eq!(settings.location(), "europe-west4");
        assert_eq!(settings.machine_type.as_deref(), Some("n2-standard-4"));
        assert!(settings.spot && settings.private_address);
        assert_eq!(
            (settings.spot_attempts, settings.boot_disk_gb),
            (3, Some(60))
        );
        assert_eq!(settings.retry_codes, [PREEMPTED]);
        assert_eq!(settings.network_tags, ["uwe"]);
        assert_eq!(settings.poll, Duration::from_secs(5));
        assert_eq!(settings.helper, Some(Helper::Path("/opt/uwe/uwe".into())));
        assert_eq!(settings.batch_endpoint, "http://127.0.0.1:1");
        assert_eq!(settings.logging_endpoint, "https://logging.googleapis.com");
        assert_eq!(Settings::default().location(), LOCATION);
        for wrong in [
            "spot = true\npreemptible = true",
            "max_spot_attempts = 11",
            "boot_disk_gb = 0",
            "logs_path = '/logs'",
            "task_folder = 'tmp'",
            "batch_endpoint = 'batch.googleapis.com'",
            "helper_url = 'https://x/uwe'",
        ] {
            let error = read(&format!("[executor.google-batch]\n{wrong}\n"));
            assert!(error.is_err(), "{wrong}");
        }
    }
}
