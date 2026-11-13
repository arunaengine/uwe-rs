//! Reads the settings of the wr executor from `[executor.wr]`: the deployment of the manager,
//! its address, the files of its token and CA certificate, and polling.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::time::Duration;

use config_file::{ConfigError, Table};

/// How often the manager is asked about the jobs, by default: wr answers from memory.
const POLL: Duration = Duration::from_secs(1);

/// The settings of the executor. Nextflow's `executor.wr` scope fills those that the config file
/// of uwe leaves unset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// `production` or `development`, which picks the folder and port of the manager.
    pub deployment: String,
    /// The address of the manager's REST API; else `https://localhost` at its default port.
    pub endpoint: Option<String>,
    /// The file of the bearer token; else `~/.wr_<deployment>/client.token`.
    pub token_file: Option<PathBuf>,
    /// The CA certificate of the manager; else `~/.wr_<deployment>/ca.pem`.
    pub ca_file: Option<PathBuf>,
    pub poll: Duration,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            deployment: "production".into(),
            endpoint: None,
            token_file: None,
            ca_file: None,
            poll: POLL,
        }
    }
}

impl Settings {
    pub const KEYS: &'static [&'static str] = &[
        "deployment",
        "endpoint",
        "token_file",
        "ca_file",
        "poll_seconds",
    ];

    /// The settings of `[executor.wr]`.
    pub fn read(table: &Table<'_>) -> Result<Settings, ConfigError> {
        let mut settings = Settings {
            endpoint: table.text("endpoint")?,
            token_file: table.text("token_file")?.map(PathBuf::from),
            ca_file: table.text("ca_file")?.map(PathBuf::from),
            ..Settings::default()
        };
        if let Some(deployment) = table.text("deployment")? {
            settings.deployment =
                deployment_name(&deployment).map_err(|text| table.invalid("deployment", text))?;
        }
        if settings
            .endpoint
            .as_ref()
            .is_some_and(|url| !url.starts_with("https://"))
        {
            return Err(table.invalid("endpoint", "must be an https:// address, as wr has it"));
        }
        if let Some(seconds) = table.integer("poll_seconds")? {
            if !(1..=3600).contains(&seconds) {
                return Err(table.invalid("poll_seconds", "must be between 1 and 3600"));
            }
            settings.poll = Duration::from_secs(seconds as u64);
        }
        Ok(settings)
    }

    /// The address of the REST API: the one of the settings, or the web port that wr picks for
    /// the user `uid` and the deployment.
    pub fn address(&self, uid: u32) -> String {
        match &self.endpoint {
            Some(endpoint) => endpoint.trim_end_matches('/').to_owned(),
            None => {
                let extra = if self.deployment == "development" {
                    2
                } else {
                    0
                };
                let port = 1021 + 4 * u64::from(uid) + 1 + extra;
                format!("https://localhost:{port}")
            }
        }
    }

    /// The token file and the CA certificate, by default in the folder of the deployment in
    /// `home`.
    pub fn files(&self, home: &Path) -> (PathBuf, PathBuf) {
        let folder = home.join(format!(".wr_{}", self.deployment));
        let token = (self.token_file.clone()).unwrap_or_else(|| folder.join("client.token"));
        let ca = (self.ca_file.clone()).unwrap_or_else(|| folder.join("ca.pem"));
        (token, ca)
    }
}

pub fn deployment_name(name: &str) -> Result<String, String> {
    match name {
        "production" | "development" => Ok(name.to_owned()),
        _ => Err(format!("is `{name}`; use production or development")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use config_file::ConfigFile;

    fn read(text: &str) -> Result<Settings, ConfigError> {
        let config = ConfigFile::parse(text, Path::new("uwe.toml"))?;
        let section = config.section("executor")?.expect("a section");
        Settings::read(&section.table("wr")?.expect("a table"))
    }

    #[test]
    fn reads_wr_settings() {
        let settings = read("[executor.wr]\ndeployment = 'development'\npoll_seconds = 5\n");
        let settings = settings.expect("settings");
        assert_eq!(settings.address(1000), "https://localhost:5024");
        let (token, ca) = settings.files(Path::new("/home/lab"));
        assert_eq!(token, Path::new("/home/lab/.wr_development/client.token"));
        assert_eq!(ca, Path::new("/home/lab/.wr_development/ca.pem"));
        assert_eq!(settings.poll, Duration::from_secs(5));
        assert_eq!(Settings::default().address(1000), "https://localhost:5022");
        let text = "[executor.wr]\nendpoint = 'https://wr.example.org:443/'\n\
                    token_file = '/run/wr/token'\nca_file = '/run/wr/ca.pem'\n";
        let settings = read(text).expect("settings");
        assert_eq!(settings.address(0), "https://wr.example.org:443");
        assert_eq!(settings.files(Path::new("/")).0, Path::new("/run/wr/token"));
        for wrong in [
            "deployment = 'test'",
            "endpoint = 'http://localhost:1022'",
            "poll_seconds = 0",
        ] {
            assert!(
                read(&format!("[executor.wr]\n{wrong}\n")).is_err(),
                "{wrong}"
            );
        }
    }
}
