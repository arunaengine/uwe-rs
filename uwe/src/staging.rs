//! Reads how the SLURM and Kubernetes executors move the files of tasks, and opens their staging
//! root.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::Duration;

use config_file::{ConfigError, Table};
use content_store::Store;
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
        Ok(Some(RemoteStage {
            objects: objects.map_err(|error| error.to_string())?,
            expiry: Duration::from_secs(self.presign_hours * 3600),
            max_outputs: self.max_outputs,
            credentials: self.credentials,
            cleanup: self.cleanup,
        }))
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
