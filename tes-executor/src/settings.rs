//! Reads generic TES authentication and staging settings from configuration.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use config_file::{ConfigError, Secret, Table};

/// Tags that uwe sets on every task itself, so `tags` may not name them. The `idempotency_tag`
/// is refused as well.
const RESERVED_TAGS: [&str; 2] = ["uwe-attempt", "uwe-submission"];

#[derive(Clone, Debug)]
pub struct Settings {
    pub staging_root: Option<String>,
    pub helper_file: Option<executor::binary::Binary>,
    pub idempotency_tag: Option<String>,
    pub user: Option<Secret>,
    pub password: Option<Secret>,
    pub max_inputs: usize,
    pub max_outputs: usize,
    /// How many minutes longer than its task a TES task may run (`staging_minutes`), for its
    /// image pull and staging; `None` for the default of [`Settings::margin`].
    pub staging_minutes: Option<u64>,
    /// Whether the objects of every finished attempt go away from the staging root
    /// (`staging_cleanup = true`), once their results are collected or abandoned.
    pub staging_cleanup: bool,
    /// Tags that go on every task (`tags`), like the ones a server reads for network access.
    pub tags: BTreeMap<String, String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            staging_root: None,
            helper_file: None,
            idempotency_tag: None,
            user: None,
            password: None,
            max_inputs: 512,
            max_outputs: 1024,
            staging_minutes: None,
            staging_cleanup: false,
            tags: BTreeMap::new(),
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
        "helper_file",
        "idempotency_tag",
        "user",
        "password",
        "max_inputs",
        "max_outputs",
        "staging_minutes",
        "staging_cleanup",
        "tags",
    ];

    /// How much longer than its task a TES task may run, as its state stays RUNNING while the
    /// server uploads its outputs: 30 minutes with a staging root, else 5, unless set.
    pub fn margin(&self) -> std::time::Duration {
        let minutes = match (self.staging_minutes, &self.staging_root) {
            (Some(minutes), _) => minutes,
            (None, None) => 5,
            (None, Some(_)) => 30,
        };
        std::time::Duration::from_secs(minutes * 60)
    }

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
            .is_some_and(|tag| tag.is_empty() || tag == "uwe-attempt")
        {
            return Err(table.invalid(
                "idempotency_tag",
                "must be nonempty and differ from uwe-attempt",
            ));
        }
        let staging_minutes = match table.integer("staging_minutes")? {
            Some(minutes @ 0..=10_080) => Some(minutes as u64),
            None => None,
            Some(_) => return Err(table.invalid("staging_minutes", "must be between 0 and 10080")),
        };
        let staging_cleanup = table.flag("staging_cleanup")?.unwrap_or(false);
        if staging_cleanup && staging_root.is_none() {
            return Err(table.invalid("staging_cleanup", "requires staging = \"object-store\""));
        }
        let mut tags = BTreeMap::new();
        if let Some(table) = table.table("tags")? {
            for key in table.keys() {
                if key.is_empty() {
                    return Err(table.invalid(key, "needs a nonempty name"));
                }
                if RESERVED_TAGS.contains(&key) || idempotency_tag.as_deref() == Some(key) {
                    return Err(table.invalid(key, "is a tag that uwe sets itself"));
                }
                tags.insert(key.to_owned(), table.text(key)?.unwrap_or_default());
            }
        }
        Ok(Self {
            helper_file: table
                .text("helper_file")?
                .map(|file| {
                    executor::binary::Binary::open(std::path::Path::new(&file))
                        .map_err(|error| table.invalid("helper_file", error))
                })
                .transpose()?,
            staging_cleanup,
            staging_minutes,
            staging_root,
            idempotency_tag,
            user,
            password,
            max_inputs: limit("max_inputs", 512)?,
            max_outputs: limit("max_outputs", 1024)?,
            tags,
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
    fn reads_staging_margin() {
        let shared = read("[executor.tes]").unwrap();
        assert_eq!(shared.margin().as_secs(), 300);
        let text = "[executor.tes]\nstaging = 'object-store'\nstaging_root = 's3://bucket/tes'";
        assert_eq!(read(text).unwrap().margin().as_secs(), 1800);
        let set = read(&format!("{text}\nstaging_minutes = 2")).unwrap();
        assert_eq!(set.margin().as_secs(), 120);
        assert!(read("[executor.tes]\nstaging_minutes = -1").is_err());
        assert!(
            read(&format!("{text}\nstaging_cleanup = true"))
                .unwrap()
                .staging_cleanup
        );
        assert!(read("[executor.tes]\nstaging_cleanup = true").is_err());
    }

    #[test]
    fn bounds_file_counts() {
        assert!(read("[executor.tes]\nmax_inputs = 513").is_err());
        assert!(read("[executor.tes]\nmax_outputs = 0").is_err());
        assert!(read("[executor.tes]\nhelper_file = '/missing/uwe'").is_err());
    }

    #[test]
    fn refuses_reserved_tags() {
        let settings = read("[executor.tes]\ntags = { \"aruna-engine.org/network\" = \"open\" }");
        let tags = settings.unwrap().tags;
        assert_eq!(tags["aruna-engine.org/network"], "open");
        let error = read("[executor.tes]\ntags = { uwe-submission = \"x\" }").unwrap_err();
        assert!(error.to_string().contains("uwe sets itself"), "{error}");
        let text = "[executor.tes]\nidempotency_tag = \"key\"\ntags = { key = \"x\" }";
        assert!(read(text).is_err());
        assert!(read("[executor.tes]\ntags = { a = 1 }").is_err());
    }

    #[test]
    fn separates_site_tags() {
        let text = "[executor.tes]\ntags = { a = \"1\" }\n\
                    [executor.tes.sites.hh]\nserver = \"https://hh\"\n\
                    [executor.tes.sites.hh.tags]\nb = \"2\"\n";
        let config = ConfigFile::parse(text, std::path::Path::new("tes.toml")).unwrap();
        let tes = config.section("executor").unwrap().unwrap();
        let tes = tes.table("tes").unwrap().unwrap();
        let site = tes.table("sites").unwrap().unwrap().tables().unwrap();
        let site = Settings::read(&site[0]).unwrap();
        assert_eq!(site.tags.keys().collect::<Vec<_>>(), ["b"]);
        assert_eq!(
            Settings::read(&tes)
                .unwrap()
                .tags
                .keys()
                .collect::<Vec<_>>(),
            ["a"]
        );
        assert!(read("[executor.tes]\ntags = { \"\" = \"x\" }").is_err());
    }
}
