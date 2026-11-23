//! Reads how the SLURM and Kubernetes executors move the files of tasks, and opens their staging
//! root.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::Duration;

use config_file::{ConfigError, Table};
use content_store::Store;
use executor::multipart::Multipart;
use executor::remote::RemoteStage;
use storage::{Storage, StoreLocation};

/// How the nodes get the inputs of tasks and give back their outputs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// The nodes see the store and scratch folders at the same paths as this machine.
    #[default]
    SharedPath,
    /// The files go through the staging root, with presigned requests.
    ObjectStore,
    /// The kubedal CSI driver places the inputs and uploads the outputs.
    Kubedal,
}

/// The staging settings of an executor, from `staging`, `staging_root`, `staging_access`,
/// `staging_cleanup`, `staging_minutes`, `presign_hours` and `max_outputs`.
#[derive(Clone, Debug)]
pub struct Settings {
    pub mode: Mode,
    /// The address of the staging root, like `s3://bucket/prefix`.
    pub root: Option<String>,
    /// How long presigned requests work: the time that a task waits in the queue and runs.
    pub presign_hours: u64,
    /// How many output files a task may give back.
    pub max_outputs: usize,
    /// Whether the nodes reach the staging root with credentials of their own instead of with
    /// presigned requests (`staging_access = "credentials"`).
    pub credentials: bool,
    /// Whether the objects of an attempt go away once its results are collected
    /// (`staging_cleanup = true`).
    pub cleanup: bool,
    /// How many minutes longer than its task a job or pod may run (`staging_minutes`); `None`
    /// for the default of [`Settings::margin`].
    pub minutes: Option<u64>,
    /// Whether output files above `multipart_threshold_mb` go to an S3 staging root in parts
    /// that this machine presigns (`multipart = true`), past the 5 GiB of one upload.
    pub multipart: bool,
    /// The size in bytes above which an output file goes up in parts; `None` for the default.
    pub multipart_threshold: Option<u64>,
    /// How many output files of an attempt, and parts of each, may go up in parts.
    pub multipart_files: Option<usize>,
    pub multipart_parts: Option<usize>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            mode: Mode::SharedPath,
            root: None,
            presign_hours: MOST_HOURS,
            max_outputs: 1024,
            credentials: false,
            cleanup: false,
            minutes: None,
            multipart: false,
            multipart_threshold: None,
            multipart_files: None,
            multipart_parts: None,
        }
    }
}

/// The longest time that S3 signs a request for: seven days.
const MOST_HOURS: u64 = 7 * 24;

impl Settings {
    pub const KEYS: &'static [&'static str] = &[
        "staging",
        "staging_root",
        "staging_access",
        "staging_cleanup",
        "staging_minutes",
        "presign_hours",
        "max_outputs",
        "multipart",
        "multipart_threshold_mb",
        "multipart_files",
        "multipart_parts",
    ];

    /// The settings of a table, whose `staging` may be one of `modes`.
    pub fn read(table: &Table<'_>, modes: &[&str]) -> Result<Settings, ConfigError> {
        let mut settings = Settings::default();
        let mode = table.text("staging")?;
        settings.mode = match mode.as_deref() {
            None | Some("shared-path") => Mode::SharedPath,
            Some(mode) if !modes.contains(&mode) => {
                return Err(table.invalid("staging", format!("must be {}", modes.join(" or "))));
            }
            Some("object-store") => Mode::ObjectStore,
            Some(_) => Mode::Kubedal,
        };
        settings.root = table.text("staging_root")?;
        match (settings.mode, &settings.root) {
            (Mode::SharedPath, Some(_)) => {
                return Err(table.invalid("staging_root", "needs staging = \"object-store\""));
            }
            (Mode::ObjectStore | Mode::Kubedal, None) => {
                return Err(table.missing("staging_root", "the address of the staging root"));
            }
            _ => {}
        }
        settings.credentials = match table.text("staging_access")?.as_deref() {
            None | Some("presigned") => false,
            Some("credentials") if settings.mode == Mode::ObjectStore => true,
            Some("credentials") => {
                let message = "needs staging = \"object-store\"";
                return Err(table.invalid("staging_access", message));
            }
            Some(_) => {
                return Err(table.invalid("staging_access", "must be presigned or credentials"));
            }
        };
        settings.cleanup = table.flag("staging_cleanup")?.unwrap_or(false);
        if settings.cleanup && settings.mode == Mode::SharedPath {
            return Err(table.invalid("staging_cleanup", "needs a staging root"));
        }
        if let Some(hours) = table.integer("presign_hours")? {
            if !(1..=MOST_HOURS as i64).contains(&hours) {
                let message =
                    format!("must be between 1 and {MOST_HOURS}, the most that S3 allows");
                return Err(table.invalid("presign_hours", message));
            }
            settings.presign_hours = hours as u64;
        }
        if let Some(minutes) = table.integer("staging_minutes")? {
            if !(0..=MOST_HOURS as i64 * 60).contains(&minutes) {
                let message = format!("must be between 0 and {}", MOST_HOURS * 60);
                return Err(table.invalid("staging_minutes", message));
            }
            settings.minutes = Some(minutes as u64);
        }
        if let Some(count) = table.integer("max_outputs")? {
            if !(1..=100_000).contains(&count) {
                return Err(table.invalid("max_outputs", "must be between 1 and 100000"));
            }
            settings.max_outputs = count as usize;
        }
        settings.multipart = table.flag("multipart")?.unwrap_or(false);
        let given = [
            "multipart_threshold_mb",
            "multipart_files",
            "multipart_parts",
        ];
        for key in given {
            if table.integer(key)?.is_some() && !settings.multipart {
                return Err(table.invalid(key, "needs multipart = true"));
            }
        }
        if settings.multipart
            && (settings.mode != Mode::ObjectStore
                || settings.credentials
                || !settings
                    .root
                    .as_deref()
                    .is_some_and(|root| root.starts_with("s3://")))
        {
            let message = "needs staging = \"object-store\" with presigned access to an s3:// root";
            return Err(table.invalid("multipart", message));
        }
        // An S3 part holds 5 MiB to 5 GiB; one upload holds at most 5 GiB.
        let limited = |key: &str, range: std::ops::RangeInclusive<i64>| -> Result<_, ConfigError> {
            match table.integer(key)? {
                Some(value) if range.contains(&value) => Ok(Some(value as u64)),
                Some(_) => Err(table.invalid(
                    key,
                    format!("must be between {} and {}", range.start(), range.end()),
                )),
                None => Ok(None),
            }
        };
        settings.multipart_threshold =
            limited("multipart_threshold_mb", 5..=5 * 1024)?.map(|mb| mb << 20);
        settings.multipart_files = limited("multipart_files", 1..=64)?.map(|count| count as usize);
        settings.multipart_parts =
            limited("multipart_parts", 1..=10_000)?.map(|count| count as usize);
        Ok(settings)
    }

    /// How much longer than its task a job or pod may run, for image pulls, staging and the
    /// start of its container: 30 minutes with a staging root, else 5, unless set.
    pub fn margin(&self) -> Duration {
        let minutes = match (self.minutes, self.mode) {
            (Some(minutes), _) => minutes,
            (None, Mode::SharedPath) => 5,
            (None, _) => 30,
        };
        Duration::from_secs(minutes * 60)
    }

    /// The staging root through the storage providers, or `None` for shared paths.
    pub fn stage(&self, storage: &Storage) -> Result<Option<RemoteStage>, String> {
        let Some(root) = self
            .root
            .as_deref()
            .filter(|_| self.mode != Mode::SharedPath)
        else {
            return Ok(None);
        };
        let objects = match storage.store(root).map_err(|error| error.to_string())? {
            StoreLocation::Objects(operator) => Store::remote(operator, root),
            StoreLocation::Folder(folder) => Store::local(&folder),
        };
        let multipart = match self.multipart {
            true => Some(self.multipart(storage, root)?),
            false => None,
        };
        Ok(Some(RemoteStage {
            objects: objects.map_err(|error| error.to_string())?,
            expiry: Duration::from_secs(self.presign_hours * 3600),
            max_outputs: self.max_outputs,
            credentials: self.credentials,
            cleanup: self.cleanup,
            multipart,
        }))
    }

    /// The multipart uploads into the S3 staging root `root`, through the service, region and
    /// keys of the S3 provider that serves it, else AWS with the keys of its default chain.
    fn multipart(&self, storage: &Storage, root: &str) -> Result<Multipart, String> {
        let s3 = storage.s3_settings(root).unwrap_or_default();
        let region = s3.region.clone().unwrap_or_else(|| "us-east-1".to_owned());
        let endpoint =
            (s3.endpoint.clone()).unwrap_or_else(|| format!("https://s3.{region}.amazonaws.com"));
        let reveal = |secret: &Option<config_file::Secret>| {
            secret.as_ref().map(|secret| secret.reveal()).transpose()
        };
        let failed = |error: config_file::ConfigError| error.to_string();
        let keys = match (
            reveal(&s3.access_key_id).map_err(failed)?,
            reveal(&s3.secret_access_key).map_err(failed)?,
        ) {
            (Some(id), Some(secret)) => storage::aws::Keys {
                id,
                secret,
                token: reveal(&s3.session_token).map_err(failed)?,
            },
            _ => {
                let chain = storage::aws::Credentials::Chain {
                    environment: std::env::vars().collect(),
                    home: std::env::var_os("HOME").map(Into::into),
                };
                let found = storage::aws::resolve(&chain);
                found.map_err(|error| format!("multipart staging needs S3 keys: {error}"))?
            }
        };
        let mut multipart = Multipart::new(&endpoint, &region, root, keys)?;
        // An endpoint of the provider names the bucket in the path, as its other requests do.
        multipart.virtual_host = s3.endpoint.is_none() && s3.path_style != Some(true);
        multipart.threshold = self.multipart_threshold.unwrap_or(multipart.threshold);
        multipart.files = self.multipart_files.unwrap_or(multipart.files);
        multipart.parts = self.multipart_parts.unwrap_or(multipart.parts);
        Ok(multipart)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use config_file::ConfigFile;

    fn read(text: &str) -> Result<Settings, ConfigError> {
        let config = ConfigFile::parse(text, std::path::Path::new("uwe.toml"))?;
        let section = config.section("executor")?.expect("a section");
        Settings::read(
            &section.table("slurm")?.expect("a table"),
            &["object-store"],
        )
    }

    #[test]
    fn configures_multipart() {
        let root = "[executor.slurm]\nstaging = 'object-store'\nstaging_root = 's3://bucket/uwe'\n";
        assert!(!read(root).unwrap().multipart, "off unless asked");
        assert!(read(&format!("{root}multipart_parts = 8\n")).is_err());
        let shared = "[executor.slurm]\nmultipart = true\n";
        assert!(read(shared).is_err(), "multipart needs an S3 staging root");
        let credentials = format!("{root}staging_access = 'credentials'\nmultipart = true\n");
        assert!(
            read(&credentials).is_err(),
            "the nodes upload with their own keys"
        );
        let text = format!(
            "{root}multipart = true\nmultipart_threshold_mb = 100\nmultipart_files = 2\n\
             multipart_parts = 16\n"
        );
        let settings = read(&text).unwrap();
        assert!(
            read(&format!(
                "{root}multipart = true\nmultipart_threshold_mb = 1\n"
            ))
            .is_err()
        );
        let config = "[storage.lab]\ntype = 's3'\nprefixes = ['s3://bucket/']\n\
                      endpoint = 'https://s3.lab.example.org'\nregion = 'eu-1'\n\
                      access_key_id = 'key'\nsecret_access_key = 'secret'\n";
        let config = ConfigFile::parse(config, std::path::Path::new("uwe.toml")).unwrap();
        let storage = Storage::from_config(&config).unwrap();
        let stage = settings.stage(&storage).unwrap().expect("a staging root");
        let multipart = stage.multipart.expect("multipart uploads");
        assert_eq!(multipart.endpoint, "https://s3.lab.example.org");
        assert_eq!(
            (multipart.region.as_str(), multipart.bucket.as_str()),
            ("eu-1", "bucket")
        );
        assert_eq!(multipart.root, "uwe");
        assert!(
            !multipart.virtual_host,
            "a provider's endpoint takes the bucket in the path"
        );
        assert_eq!(multipart.threshold, 100 << 20);
        assert_eq!((multipart.files, multipart.parts), (2, 16));
        assert_eq!(multipart.keys.id, "key");
    }

    #[test]
    fn reads_staging_root() {
        let settings = read("[executor.slurm]").unwrap();
        assert_eq!(settings.mode, Mode::SharedPath);
        assert!(read("[executor.slurm]\nstaging = 'object-store'").is_err());
        assert!(read("[executor.slurm]\nstaging = 'kubedal'\nstaging_root = 's3://b'").is_err());
        assert!(read("[executor.slurm]\nstaging_root = 's3://b'").is_err());
        let text = "[executor.slurm]\nstaging = 'object-store'\nstaging_root = 's3://b/p'\n\
                    presign_hours = 48";
        let settings = read(text).unwrap();
        assert_eq!(settings.mode, Mode::ObjectStore);
        assert_eq!(settings.root.as_deref(), Some("s3://b/p"));
        assert_eq!(settings.presign_hours, 48);
        assert!(read(&format!("{text}0")).is_err(), "more than seven days");
        assert!(!settings.credentials);
        let settings = read(&format!("{text}\nstaging_access = 'credentials'")).unwrap();
        assert!(settings.credentials);
        assert!(read(&format!("{text}\nstaging_access = 'keys'")).is_err());
        let shared = "[executor.slurm]\nstaging_access = 'credentials'";
        assert!(
            read(shared).is_err(),
            "credentials without object-store staging"
        );
        assert!(
            read(&format!("{text}\nstaging_cleanup = true"))
                .unwrap()
                .cleanup
        );
        assert!(read("[executor.slurm]\nstaging_cleanup = true").is_err());
        assert_eq!(settings.margin(), Duration::from_secs(1800));
        let shared = read("[executor.slurm]").unwrap();
        assert_eq!(shared.margin(), Duration::from_secs(300));
        let set = read(&format!("{text}\nstaging_minutes = 90")).unwrap();
        assert_eq!(set.margin(), Duration::from_secs(5400));
        assert!(read(&format!("{text}\nstaging_minutes = -1")).is_err());
    }
}
