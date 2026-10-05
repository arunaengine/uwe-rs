//! Reads generic TES authentication and staging settings from configuration.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use config_file::{ConfigError, Secret, Table};

#[derive(Clone, Debug)]
pub struct Settings {
    pub staging_root: Option<String>,
    pub idempotency_tag: Option<String>,
    pub user: Option<Secret>,
    pub password: Option<Secret>,
    pub max_inputs: usize,
    pub max_outputs: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            staging_root: None,
            idempotency_tag: None,
            user: None,
            password: None,
            max_inputs: 512,
            max_outputs: 1024,
        }
    }
}

impl Settings {
    pub fn client(&self, server: &str, token: Option<String>) -> Result<crate::Client, String> {
        let bearer = token.is_some();
        let mut client = crate::Client::new(server, token).map_err(|error| error.to_string())?;
        if !bearer && let (Some(user), Some(password)) = (&self.user, &self.password) {
            client = client.with_basic(
                user.reveal().map_err(|error| error.to_string())?,
                password.reveal().map_err(|error| error.to_string())?,
            );
        }
        Ok(client)
    }

    pub const KEYS: &'static [&'static str] = &[
        "staging",
        "staging_root",
        "idempotency_tag",
        "user",
        "password",
        "max_inputs",
        "max_outputs",
    ];

    pub fn read(table: &Table<'_>) -> Result<Self, ConfigError> {
        let staging = table
            .text("staging")?
            .unwrap_or_else(|| "shared-path".into());
        let root = table.text("staging_root")?;
        let staging_root = match staging.as_str() {
            "shared-path" if root.is_none() => None,
            "object-store" => {
                let root = root.ok_or_else(|| {
                    table.missing("staging_root", "an S3 address for TES staging")
                })?;
                let rest = root.strip_prefix("s3://").unwrap_or_default();
                if rest.is_empty()
                    || rest.split('/').next().unwrap_or("").contains(['@', ':'])
                    || rest.chars().any(char::is_whitespace)
                    || rest.starts_with('/')
                    || rest.contains(['?', '#', '\\'])
                    || rest
                        .trim_end_matches('/')
                        .split('/')
                        .any(|part| part.is_empty() || part == "." || part == "..")
                {
                    return Err(table.invalid(
                        "staging_root",
                        "must be s3://bucket/prefix without credentials or query parameters",
                    ));
                }
                Some(root.trim_end_matches('/').to_owned())
            }
            "shared-path" => {
                return Err(table.invalid("staging_root", "requires staging = \"object-store\""));
            }
            _ => return Err(table.invalid("staging", "must be shared-path or object-store")),
        };
        let user = table.secret("user")?;
        let password = table.secret("password")?;
        if user.is_some() != password.is_some() {
            return Err(table.invalid(
                "user",
                "HTTP Basic authentication needs both user and password",
            ));
        }
        let limit = |key, maximum| match table.integer(key)? {
            Some(value) if value > 0 && value <= maximum => Ok(value as usize),
            None => Ok(maximum as usize),
            _ => Err(table.invalid(key, format!("must be between 1 and {maximum}"))),
        };
        let idempotency_tag = table.text("idempotency_tag")?;
        if idempotency_tag
            .as_deref()
            .is_some_and(|tag| tag.is_empty() || tag == "iunctus-attempt")
        {
            return Err(table.invalid(
                "idempotency_tag",
                "must be nonempty and differ from iunctus-attempt",
            ));
        }
        Ok(Self {
            staging_root,
            idempotency_tag,
            user,
            password,
            max_inputs: limit("max_inputs", 512)?,
            max_outputs: limit("max_outputs", 1024)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use config_file::ConfigFile;

    fn read(text: &str) -> Result<Settings, ConfigError> {
        let config = ConfigFile::parse(text, std::path::Path::new("tes.toml"))?;
        Settings::read(&config.section("executor")?.unwrap().table("tes")?.unwrap())
    }

    #[test]
    fn requires_staging_root() {
        assert!(read("[executor.tes]\nstaging = 'object-store'").is_err());
        let settings =
            read("[executor.tes]\nstaging = 'object-store'\nstaging_root = 's3://bucket/tes'")
                .unwrap();
        assert_eq!(settings.staging_root.as_deref(), Some("s3://bucket/tes"));
        assert!(settings.idempotency_tag.is_none());
    }

    #[test]
    fn bounds_file_counts() {
        assert!(read("[executor.tes]\nmax_inputs = 513").is_err());
        assert!(read("[executor.tes]\nmax_outputs = 0").is_err());
    }
}
