//! Reads the settings of the JARVICE executor from `[executor.jarvice]`: the API, the user and
//! its key, the machine type, the vault, polling and the uwe helper.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::Duration;

use config_file::{ConfigError, Secret, Table};
use executor::batch::{Helper, TASK_FOLDER, helper};

/// The API of the Nimbix cloud, which the JARVICE documentation names.
pub const API_URL: &str = "https://cloud.nimbix.net/api";

/// Where JARVICE jobs see their vault.
pub const VAULT_PATH: &str = "/data";

/// How often JARVICE is asked about a job, by default, as nf-jarvice does.
const POLL: Duration = Duration::from_secs(10);

/// The settings of the executor. Nextflow's process settings fill those that the config file
/// of uwe leaves unset.
#[derive(Clone, Debug)]
pub struct Settings {
    pub api_url: String,
    pub username: Option<String>,
    /// The API key, which `Debug` of [`Secret`] never shows.
    pub api_key: Option<Secret>,
    /// The machine type of tasks that name none, like `n1`.
    pub machine_type: Option<String>,
    /// The vault that the jobs mount, and how.
    pub vault: Option<String>,
    pub vault_readonly: bool,
    pub vault_force: bool,
    /// Where the jobs see the vault, which must hold the scratch folder of uwe for shared paths.
    pub vault_path: String,
    pub poll: Duration,
    /// The folder of the container that holds the folders of attempts, for object-store
    /// staging.
    pub task_folder: String,
    /// Where the jobs get the static uwe binary; `None` for one that uwe uploads.
    pub helper: Option<Helper>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            api_url: API_URL.into(),
            username: None,
            api_key: None,
            machine_type: None,
            vault: None,
            vault_readonly: false,
            vault_force: false,
            vault_path: VAULT_PATH.into(),
            poll: POLL,
            task_folder: TASK_FOLDER.into(),
            helper: None,
        }
    }
}

impl Settings {
    pub const KEYS: &'static [&'static str] = &[
        "api_url",
        "username",
        "api_key",
        "machine_type",
        "vault",
        "vault_readonly",
        "vault_force",
        "vault_path",
        "poll_seconds",
        "task_folder",
        "helper_path",
        "helper_url",
        "helper_sha256",
    ];

    /// The settings of `[executor.jarvice]`.
    pub fn read(table: &Table<'_>) -> Result<Settings, ConfigError> {
        let mut settings = Settings {
            username: table.text("username")?,
            api_key: table.secret("api_key")?,
            machine_type: table.text("machine_type")?,
            vault: table.text("vault")?,
            vault_readonly: table.flag("vault_readonly")?.unwrap_or(false),
            vault_force: table.flag("vault_force")?.unwrap_or(false),
            helper: helper(table)?,
            ..Settings::default()
        };
        if let Some(url) = table.text("api_url")? {
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return Err(table.invalid("api_url", "must be an http:// or https:// address"));
            }
            settings.api_url = url;
        }
        for (key, slot) in [
            ("vault_path", &mut settings.vault_path),
            ("task_folder", &mut settings.task_folder),
        ] {
            if let Some(folder) = table.text(key)? {
                if !folder.starts_with('/') || folder.split('/').any(|part| part == "..") {
                    return Err(table.invalid(key, "must be an absolute folder"));
                }
                *slot = folder;
            }
        }
        if let Some(seconds) = table.integer("poll_seconds")? {
            if !(1..=3600).contains(&seconds) {
                return Err(table.invalid("poll_seconds", "must be between 1 and 3600"));
            }
            settings.poll = Duration::from_secs(seconds as u64);
        }
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use config_file::ConfigFile;

    fn read(text: &str) -> Result<Settings, ConfigError> {
        let config = ConfigFile::parse(text, std::path::Path::new("uwe.toml"))?;
        let section = config.section("executor")?.expect("a section");
        Settings::read(&section.table("jarvice")?.expect("a table"))
    }

    #[test]
    fn reads_jarvice_settings() {
        let text = "[executor.jarvice]\napi_url = 'https://jarvice.example.org/api'\n\
                    username = 'lab'\napi_key = { environment = 'JARVICE_KEY_OF_TEST' }\n\
                    machine_type = 'n1'\nvault = 'persistent'\nvault_force = true\n\
                    vault_path = '/data'\npoll_seconds = 2\n";
        let settings = read(text).expect("settings");
        assert_eq!(settings.api_url, "https://jarvice.example.org/api");
        assert_eq!(settings.username.as_deref(), Some("lab"));
        assert!(settings.api_key.is_some());
        assert_eq!(settings.machine_type.as_deref(), Some("n1"));
        assert_eq!(settings.vault.as_deref(), Some("persistent"));
        assert!(settings.vault_force && !settings.vault_readonly);
        assert_eq!(settings.poll, Duration::from_secs(2));
        assert_eq!(Settings::default().api_url, API_URL);
        for wrong in [
            "api_url = 'cloud.nimbix.net'",
            "vault_path = 'data'",
            "task_folder = '/tmp/../x'",
            "poll_seconds = 0",
        ] {
            let error = read(&format!("[executor.jarvice]\n{wrong}\n"));
            assert!(error.is_err(), "{wrong}");
        }
    }
}
