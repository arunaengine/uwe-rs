//! Reads the settings of the Snowflake executor from `[executor.snowflake]` and from a
//! connection of the Snowflake drivers' `connections.toml`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::time::Duration;

use config_file::{ConfigError, ConfigFile, Secret, Table};
use executor::batch::{Helper, TASK_FOLDER, helper};

/// How often Snowflake is asked about a statement, by default.
const POLL: Duration = Duration::from_secs(5);

/// The settings of the executor. The Nextflow `snowflake` scope and a connection of
/// `connections.toml` fill those that the config file of uwe leaves unset.
#[derive(Clone, Debug)]
pub struct Settings {
    /// The account identifier, like `myorg-myaccount` or `xy12345.eu-central-1`.
    pub account: Option<String>,
    pub user: Option<String>,
    pub role: Option<String>,
    pub warehouse: Option<String>,
    /// The database and schema that hold the job services.
    pub database: Option<String>,
    pub schema: Option<String>,
    /// The address of the SQL API; else `https://<account>.snowflakecomputing.com`.
    pub host: Option<String>,
    /// The RSA key of the user for key-pair sign-in, in a PEM file.
    pub private_key_file: Option<String>,
    /// A programmatic access token, instead of a key.
    pub token: Option<Secret>,
    /// The connection of `connections.toml` that fills unset settings.
    pub connection: Option<String>,
    pub compute_pool: Option<String>,
    /// The external access integrations that let a job reach the staging root.
    pub integrations: Vec<String>,
    /// Image prefixes and their replacements, as nf-snowflake's `registryMappings`.
    pub mappings: Vec<(String, String)>,
    pub poll: Duration,
    /// The folder of the container that holds the folders of attempts.
    pub task_folder: String,
    /// Where the jobs get the static uwe binary; `None` for one that uwe uploads.
    pub helper: Option<Helper>,
    /// An image in a Snowflake image repository that holds the static uwe binary at `/uwe`.
    pub helper_image: Option<String>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            account: None,
            user: None,
            role: None,
            warehouse: None,
            database: None,
            schema: None,
            host: None,
            private_key_file: None,
            token: None,
            connection: None,
            compute_pool: None,
            integrations: Vec::new(),
            mappings: Vec::new(),
            poll: POLL,
            task_folder: TASK_FOLDER.into(),
            helper: None,
            helper_image: None,
        }
    }
}

impl Settings {
    pub const KEYS: &'static [&'static str] = &[
        "account",
        "user",
        "role",
        "warehouse",
        "database",
        "schema",
        "host",
        "private_key_file",
        "token",
        "connection_name",
        "compute_pool",
        "external_access_integrations",
        "registry_mappings",
        "poll_seconds",
        "task_folder",
        "helper_path",
        "helper_url",
        "helper_sha256",
        "helper_image",
    ];

    /// The settings of `[executor.snowflake]`.
    pub fn read(table: &Table<'_>) -> Result<Settings, ConfigError> {
        let mut settings = Settings {
            account: table.text("account")?,
            user: table.text("user")?,
            role: table.text("role")?,
            warehouse: table.text("warehouse")?,
            database: table.text("database")?,
            schema: table.text("schema")?,
            host: table.text("host")?,
            private_key_file: table.text("private_key_file")?,
            token: table.secret("token")?,
            connection: table.text("connection_name")?,
            compute_pool: table.text("compute_pool")?,
            integrations: table
                .texts("external_access_integrations")?
                .unwrap_or_default(),
            helper: helper(table)?,
            helper_image: table.text("helper_image")?,
            ..Settings::default()
        };
        if settings.helper.is_some() && settings.helper_image.is_some() {
            let message = "takes no helper_path or helper_url; name one place for the binary";
            return Err(table.invalid("helper_image", message));
        }
        let names = (settings.compute_pool.iter()).chain(&settings.integrations);
        if let Some(name) = names.into_iter().find(|name| !identifier(name)) {
            let message = format!("has `{name}`, which is no Snowflake identifier");
            return Err(table.invalid("compute_pool", message));
        }
        if let Some(text) = table.text("registry_mappings")? {
            settings.mappings =
                mappings(&text).map_err(|message| table.invalid("registry_mappings", message))?;
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
        Ok(settings)
    }

    /// Fills the unset settings from the connection `name` of the `connections.toml` at
    /// `file`, as the Snowflake drivers name its keys; false when the file has no such one.
    pub fn connect(&mut self, file: &Path, name: &str) -> Result<bool, String> {
        let text = std::fs::read_to_string(file)
            .map_err(|error| format!("cannot read {}: {error}", file.display()))?;
        let config = ConfigFile::parse(&text, file).map_err(|error| error.to_string())?;
        let Some(table) = config.section(name).map_err(|error| error.to_string())? else {
            return Ok(false);
        };
        let text = |key: &str| table.text(key).map_err(|error| error.to_string());
        for (slot, key) in [
            (&mut self.account, "account"),
            (&mut self.user, "user"),
            (&mut self.role, "role"),
            (&mut self.warehouse, "warehouse"),
            (&mut self.database, "database"),
            (&mut self.schema, "schema"),
            (&mut self.host, "host"),
        ] {
            if slot.is_none() {
                *slot = text(key)?;
            }
        }
        if self.private_key_file.is_none() {
            self.private_key_file = text("private_key_file")?.or(text("private_key_path")?);
        }
        if self.token.is_none() {
            self.token = table.secret("token").map_err(|error| error.to_string())?;
        }
        Ok(true)
    }
}

/// The `connections.toml` of the Snowflake drivers: in `~/.snowflake` when that folder exists,
/// else in `$SNOWFLAKE_HOME`, else in `~/.config/snowflake`.
pub fn connections_file(home: Option<&Path>, variable: Option<&Path>) -> Option<PathBuf> {
    let own = home.map(|home| home.join(".snowflake"));
    match (own, variable) {
        (Some(own), _) if own.is_dir() => Some(own.join("connections.toml")),
        (_, Some(variable)) => Some(variable.join("connections.toml")),
        _ => home.map(|home| home.join(".config/snowflake/connections.toml")),
    }
}

/// Whether `name` is a Snowflake identifier: plain letters, digits, `_` and `$` that start with
/// a letter or `_`, or a quoted one without quotes inside, each part of a dotted name.
pub fn identifier(name: &str) -> bool {
    !name.is_empty()
        && name.split('.').all(|part| match part.strip_prefix('"') {
            Some(quoted) => {
                quoted.len() > 1
                    && quoted.ends_with('"')
                    && !quoted[..quoted.len() - 1].contains('"')
            }
            None => {
                let mut letters = part.chars();
                letters
                    .next()
                    .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
                    && letters.all(|letter| letter.is_ascii_alphanumeric() || "_$".contains(letter))
            }
        })
}

/// The pairs of `from:to,from:to`, as nf-snowflake's `registryMappings`.
pub fn mappings(text: &str) -> Result<Vec<(String, String)>, String> {
    let pairs = text
        .split(',')
        .map(str::trim)
        .filter(|pair| !pair.is_empty());
    pairs
        .map(|pair| match pair.split_once(':') {
            Some((from, to)) if !from.trim().is_empty() && !to.trim().is_empty() => {
                Ok((from.trim().to_owned(), to.trim().to_owned()))
            }
            _ => Err(format!(
                "has `{pair}`; use from:to pairs, like docker.io:my_repository"
            )),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> Result<Settings, ConfigError> {
        let config = ConfigFile::parse(text, Path::new("uwe.toml"))?;
        let section = config.section("executor")?.expect("a section");
        Settings::read(&section.table("snowflake")?.expect("a table"))
    }

    #[test]
    fn reads_snowflake_settings() {
        let text = "[executor.snowflake]\naccount = 'myorg-lab'\nuser = 'uwe'\n\
                    compute_pool = 'TASKS'\nexternal_access_integrations = ['STAGING']\n\
                    registry_mappings = 'docker.io:/db/images/repo, quay.io:/db/quay/repo'\n\
                    poll_seconds = 2\nhelper_image = '/db/images/repo/uwe:0.1.0'\n";
        let settings = read(text).expect("settings");
        assert_eq!(settings.account.as_deref(), Some("myorg-lab"));
        assert_eq!(settings.integrations, ["STAGING"]);
        assert_eq!(
            settings.mappings[1],
            ("quay.io".into(), "/db/quay/repo".into())
        );
        assert_eq!(settings.poll, Duration::from_secs(2));
        for wrong in [
            "compute_pool = 'a; drop table x'",
            "registry_mappings = 'docker.io'",
            "helper_image = 'x'\nhelper_path = '/opt/uwe'",
            "task_folder = 'tmp'",
        ] {
            assert!(
                read(&format!("[executor.snowflake]\n{wrong}\n")).is_err(),
                "{wrong}"
            );
        }
    }

    #[test]
    fn checks_identifiers() {
        for good in ["TASKS", "db.schema.pool_1", "\"my pool\"", "_x$"] {
            assert!(identifier(good), "{good}");
        }
        for bad in ["", "1pool", "a;b", "\"a\"b\"", "pool name", "a..b"] {
            assert!(!identifier(bad), "{bad}");
        }
    }

    #[test]
    fn reads_driver_connections() {
        let folder = tempfile::tempdir().expect("a folder");
        let file = folder.path().join("connections.toml");
        let text = "[lab]\naccount = 'myorg-lab'\nuser = 'uwe'\nrole = 'RUNNER'\n\
                    private_key_path = '/keys/uwe.p8'\n";
        std::fs::write(&file, text).expect("writes the file");
        let mut settings = Settings {
            role: Some("MINE".into()),
            ..Settings::default()
        };
        assert_eq!(settings.connect(&file, "lab"), Ok(true));
        assert_eq!(settings.account.as_deref(), Some("myorg-lab"));
        assert_eq!(
            settings.role.as_deref(),
            Some("MINE"),
            "uwe's own setting wins"
        );
        assert_eq!(settings.private_key_file.as_deref(), Some("/keys/uwe.p8"));
        assert_eq!(settings.connect(&file, "other"), Ok(false));
        let home = folder.path();
        let found = connections_file(Some(home), None);
        assert_eq!(found, Some(home.join(".config/snowflake/connections.toml")));
        let variable = connections_file(Some(home), Some(Path::new("/sf")));
        assert_eq!(variable, Some(PathBuf::from("/sf/connections.toml")));
        std::fs::create_dir(home.join(".snowflake")).expect("makes the folder");
        let own = connections_file(Some(home), Some(Path::new("/sf")));
        assert_eq!(own, Some(home.join(".snowflake/connections.toml")));
    }
}
