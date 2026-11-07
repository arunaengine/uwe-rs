//! Reads the settings of the Flyte executor from `[executor.flyte]`: the control plane, the
//! project of the runs, the API key, polling and the uwe helper.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use config_file::{ConfigError, Secret, Table};
use executor::batch::{Helper, TASK_FOLDER, helper};

/// How often Flyte is asked about a run, by default, as nf-flyte's `pollInterval`.
const POLL: Duration = Duration::from_secs(5);

/// The variables that may hold an API key, in the order that nf-flyte reads them.
pub const KEY_VARIABLES: [&str; 3] = ["_UNION_EAGER_API_KEY", "EAGER_API_KEY", "FLYTE_API_KEY"];

/// The settings of the executor. The Nextflow `flyte` scope fills those that the config file
/// of uwe leaves unset.
#[derive(Clone, Debug)]
pub struct Settings {
    /// The control plane, like `tenant.hosted.unionai.cloud`; else from the API key.
    pub endpoint: Option<String>,
    /// The organization of the runs; else from the API key.
    pub org: Option<String>,
    pub project: Option<String>,
    pub domain: Option<String>,
    /// The API key; else one of [`KEY_VARIABLES`].
    pub api_key: Option<Secret>,
    /// The queue of the runs, which Flyte takes as their cluster.
    pub queue: Option<String>,
    pub poll: Duration,
    /// The folder of the container that holds the folders of attempts and of the inputs that
    /// Flyte downloads.
    pub task_folder: String,
    /// Where the runs get the static uwe binary; `None` for one that uwe uploads.
    pub helper: Option<Helper>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            endpoint: None,
            org: None,
            project: None,
            domain: None,
            api_key: None,
            queue: None,
            poll: POLL,
            task_folder: TASK_FOLDER.into(),
            helper: None,
        }
    }
}

impl Settings {
    pub const KEYS: &'static [&'static str] = &[
        "endpoint",
        "org",
        "project",
        "domain",
        "api_key",
        "queue",
        "poll_seconds",
        "task_folder",
        "helper_path",
        "helper_url",
        "helper_sha256",
    ];

    /// The settings of `[executor.flyte]`.
    pub fn read(table: &Table<'_>) -> Result<Settings, ConfigError> {
        let mut settings = Settings {
            endpoint: table.text("endpoint")?,
            org: table.text("org")?,
            project: table.text("project")?,
            domain: table.text("domain")?,
            api_key: table.secret("api_key")?,
            queue: table.text("queue")?,
            helper: helper(table)?,
            ..Settings::default()
        };
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
        Ok(settings)
    }
}

/// The parts of a Flyte API key, which is base64 of `endpoint:client_id:client_secret:org`.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey {
    pub endpoint: String,
    pub client_id: String,
    pub secret: String,
    pub org: String,
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApiKey")
            .field("endpoint", &self.endpoint)
            .field("client_id", &self.client_id)
            .field("org", &self.org)
            .finish_non_exhaustive()
    }
}

impl ApiKey {
    /// Reads a key; the endpoint may hold colons itself, like `dns:///host:443`.
    pub fn parse(text: &str) -> Result<ApiKey, String> {
        let text = text.trim();
        let bytes = (STANDARD.decode(text))
            .or_else(|_| STANDARD_NO_PAD.decode(text))
            .map_err(|_| "the Flyte API key is not base64".to_owned())?;
        let decoded = String::from_utf8(bytes).map_err(|_| "the Flyte API key is not text")?;
        let mut parts = decoded.rsplitn(4, ':');
        let (org, secret, client_id, endpoint) =
            (parts.next(), parts.next(), parts.next(), parts.next());
        match (endpoint, client_id, secret, org) {
            (Some(endpoint), Some(client_id), Some(secret), Some(org))
                if !endpoint.is_empty() && !client_id.is_empty() && !secret.is_empty() =>
            {
                Ok(ApiKey {
                    endpoint: endpoint.to_owned(),
                    client_id: client_id.to_owned(),
                    secret: secret.to_owned(),
                    org: org.to_owned(),
                })
            }
            _ => Err("the Flyte API key does not hold endpoint:client_id:client_secret:org".into()),
        }
    }
}

/// The address of a control plane: `https://` before a bare host, without `dns:///` and a
/// trailing `/`. An `http://` address stays, as for a local Flyte.
pub fn address(endpoint: &str) -> String {
    let endpoint = endpoint
        .trim()
        .trim_start_matches("dns:///")
        .trim_end_matches('/');
    match endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        true => endpoint.to_owned(),
        false => format!("https://{endpoint}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use config_file::ConfigFile;

    fn read(text: &str) -> Result<Settings, ConfigError> {
        let config = ConfigFile::parse(text, std::path::Path::new("uwe.toml"))?;
        let section = config.section("executor")?.expect("a section");
        Settings::read(&section.table("flyte")?.expect("a table"))
    }

    #[test]
    fn reads_flyte_settings() {
        let text = "[executor.flyte]\nendpoint = 'flyte.example.org'\nproject = 'lab'\n\
                    domain = 'development'\nqueue = 'gpu'\npoll_seconds = 2\n\
                    helper_path = '/opt/uwe'\n";
        let settings = read(text).expect("settings");
        assert_eq!(settings.endpoint.as_deref(), Some("flyte.example.org"));
        assert_eq!(settings.project.as_deref(), Some("lab"));
        assert_eq!(settings.queue.as_deref(), Some("gpu"));
        assert_eq!(settings.poll, Duration::from_secs(2));
        assert_eq!(settings.helper, Some(Helper::Path("/opt/uwe".into())));
        for wrong in ["poll_seconds = 0", "task_folder = 'tmp'"] {
            assert!(
                read(&format!("[executor.flyte]\n{wrong}\n")).is_err(),
                "{wrong}"
            );
        }
    }

    #[test]
    fn reads_api_keys() {
        let key = STANDARD.encode("dns:///flyte.example.org:443:uwe-app:s3cret:lab");
        let parsed = ApiKey::parse(&key).expect("a key");
        assert_eq!(parsed.endpoint, "dns:///flyte.example.org:443");
        assert_eq!(
            (parsed.client_id.as_str(), parsed.org.as_str()),
            ("uwe-app", "lab")
        );
        assert!(!format!("{parsed:?}").contains("s3cret"));
        assert_eq!(address(&parsed.endpoint), "https://flyte.example.org:443");
        assert_eq!(address("http://127.0.0.1:1/"), "http://127.0.0.1:1");
        assert!(ApiKey::parse(&STANDARD.encode("host:id")).is_err());
        assert!(ApiKey::parse("not base64!").is_err());
    }
}
