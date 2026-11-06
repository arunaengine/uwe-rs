//! Builds Snowflake executors from `[executor.snowflake]`, a driver connection and the sign-in
//! of the user, with object-store staging and the uwe helper for the job services.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use config_file::{ConfigError, Table};
use content_store::Store;
use snowflake_executor::client::{ACCESS_TOKEN, OAUTH};
use snowflake_executor::{Auth, Client, Context, KeyPair, Settings, SnowflakeBackend};
use task_runner::{Runner, RunnerExecutor};

/// The token that Snowpark Container Services gives a container, for uwe running inside one.
const SESSION_TOKEN: &str = "/snowflake/session/token";

/// The settings of `[executor.snowflake]`: those of the job services, of the staging root, and
/// the binary that uwe uploads for the jobs.
#[derive(Clone, Debug, Default)]
pub struct Config {
    pub settings: Settings,
    pub staging: crate::staging::Settings,
    /// Without another helper setting: `helper_file`, or else the running binary.
    pub helper_file: Option<PathBuf>,
}

impl Config {
    pub fn read(table: &Table<'_>) -> Result<Config, ConfigError> {
        let mut keys = Settings::KEYS.to_vec();
        keys.push("helper_file");
        keys.extend(crate::staging::Settings::KEYS);
        table.only(&keys)?;
        let settings = Settings::read(table)?;
        let staging = crate::staging::Settings::read(table, &["object-store"])?;
        let named = settings.helper.is_some() || settings.helper_image.is_some();
        let helper_file = crate::setup::helper_file(table, named)?;
        Ok(Config {
            settings,
            staging,
            helper_file,
        })
    }
}

/// An executor that runs each task as a Snowflake job service, with its attempts in
/// `<scratch>/snowflake`. Tasks without an image get `default_image`.
pub fn executor(
    store: Store,
    scratch: &Path,
    run: &str,
    default_image: Option<String>,
    setup: &crate::setup::Setup,
) -> Result<RunnerExecutor<crate::conda_backend::Remote<SnowflakeBackend>>, String> {
    let config = &setup.executors.snowflake;
    let stage = config.staging.stage(&setup.storage)?.ok_or(
        "Snowflake job services share no folders with this machine; set staging = \
         \"object-store\" and staging_root = \"s3://bucket/folder\" in [executor.snowflake]",
    )?;
    let variable = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let mut settings = config.settings.clone();
    let named =
        (settings.connection.clone()).or_else(|| variable("SNOWFLAKE_DEFAULT_CONNECTION_NAME"));
    let home = variable("HOME").map(PathBuf::from);
    let file = snowflake_executor::connections_file(
        home.as_deref(),
        variable("SNOWFLAKE_HOME").as_deref().map(Path::new),
    );
    match (file, &named) {
        (Some(file), named) if file.is_file() => {
            settings.connect(&file, named.as_deref().unwrap_or("default"))?;
        }
        (_, Some(named)) => {
            return Err(format!(
                "the Snowflake connection `{named}` needs a connections.toml"
            ));
        }
        _ => {}
    }
    for (slot, name) in [
        (&mut settings.account, "SNOWFLAKE_ACCOUNT"),
        (&mut settings.host, "SNOWFLAKE_HOST"),
        (&mut settings.database, "SNOWFLAKE_DATABASE"),
        (&mut settings.schema, "SNOWFLAKE_SCHEMA"),
        (&mut settings.warehouse, "SNOWFLAKE_WAREHOUSE"),
    ] {
        if slot.is_none() {
            *slot = variable(name);
        }
    }
    let auth = sign_in(&settings)?;
    let address = match (&settings.host, &settings.account) {
        (Some(host), _) if host.starts_with("http://") || host.starts_with("https://") => {
            host.clone()
        }
        (Some(host), _) => format!("https://{host}"),
        (None, Some(account)) => {
            let host = account.to_ascii_lowercase().replace('_', "-");
            format!("https://{host}.snowflakecomputing.com")
        }
        (None, None) => {
            return Err("the Snowflake executor needs the account: set account in \
                        [executor.snowflake] or a connection of connections.toml"
                .into());
        }
    };
    let context = Context {
        database: settings.database.clone(),
        schema: settings.schema.clone(),
        warehouse: settings.warehouse.clone(),
        role: settings.role.clone(),
    };
    let client = Client::new(&address, auth, context);
    // A pipeline may pick Snowflake without an [executor.snowflake] that names the helper.
    let file = (config.helper_file.clone()).or_else(|| std::env::current_exe().ok());
    let named = settings.helper.is_some() || settings.helper_image.is_some();
    let upload = match (named, file) {
        (false, Some(file)) => {
            Some(executor::binary::Binary::open(&file).map_err(|error| {
                format!("{error}, or set helper_image, helper_path or helper_url")
            })?)
        }
        _ => None,
    };
    let scratch = scratch.join("snowflake");
    let mut backend = SnowflakeBackend::new(client, settings, scratch.clone(), stage, upload)?;
    backend.staging_time = config.staging.margin();
    // The jobs read the inputs from the staging root, so they are copied there.
    let backend = crate::conda_backend::Remote::new(backend, store.clone(), &scratch, run, setup);
    let settings = crate::runner_settings(setup.storage.clone(), true);
    let mut runner = Runner::new(store, backend, run, settings);
    runner.references(setup.references.clone());
    Ok(RunnerExecutor::new(runner, default_image))
}

/// How uwe signs in: with a programmatic access token, the user's key, or the token of the
/// Snowpark Container Services session it runs in.
fn sign_in(settings: &Settings) -> Result<Auth, String> {
    if let Some(token) = &settings.token {
        let token = token.reveal().map_err(|error| error.to_string())?;
        return Ok(Auth::Token {
            kind: ACCESS_TOKEN,
            token,
        });
    }
    if let Some(file) = &settings.private_key_file {
        let (Some(account), Some(user)) = (&settings.account, &settings.user) else {
            return Err("key-pair sign-in to Snowflake needs the account and the user".into());
        };
        let pem = std::fs::read_to_string(file)
            .map_err(|error| format!("cannot read the private key {file}: {error}"))?;
        return Ok(Auth::KeyPair(Box::new(KeyPair::new(account, user, &pem)?)));
    }
    match std::fs::read_to_string(SESSION_TOKEN) {
        Ok(token) => Ok(Auth::Token {
            kind: OAUTH,
            token: token.trim().to_owned(),
        }),
        Err(_) => Err(
            "the Snowflake executor needs a sign-in: set private_key_file and user, \
                       or token, in [executor.snowflake] or a connection of connections.toml"
                .into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use config_file::ConfigFile;

    fn read(text: &str) -> Result<Config, ConfigError> {
        let config = ConfigFile::parse(text, Path::new("uwe.toml"))?;
        let section = config.section("executor")?.expect("a section");
        Config::read(&section.table("snowflake")?.expect("a table"))
    }

    #[test]
    fn reads_snowflake_config() {
        let text = "[executor.snowflake]\ncompute_pool = 'TASKS'\nstaging = 'object-store'\n\
                    staging_root = 's3://bucket/uwe'\nhelper_image = '/lab/images/repo/uwe'\n";
        let config = read(text).expect("a config");
        assert_eq!(config.settings.compute_pool.as_deref(), Some("TASKS"));
        assert_eq!(config.staging.root.as_deref(), Some("s3://bucket/uwe"));
        assert_eq!(
            config.helper_file, None,
            "the helper image holds the binary"
        );
        let both = "[executor.snowflake]\nhelper_image = 'x'\nhelper_file = '/opt/uwe'\n";
        assert!(read(both).is_err(), "two places for the helper");
        assert!(read("[executor.snowflake]\npassword = 'x'\n").is_err());
    }
}
