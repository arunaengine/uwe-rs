//! Reads local channel repodata from <folder>/<channel>/<platform>/repodata.json.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::LockError;
use crate::version::Version;

/// A package of a channel, as its repodata describes it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub name: String,
    pub version: String,
    pub build: String,
    #[serde(default)]
    pub build_number: u64,
    #[serde(default)]
    pub depends: Vec<String>,
    /// Versions that other packages must have if they are installed too.
    #[serde(default)]
    pub constrains: Vec<String>,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub md5: Option<String>,
    #[serde(default)]
    pub timestamp: Option<u64>,
    /// The channel and the platform folder that hold the package, and its file name.
    #[serde(skip)]
    pub channel: String,
    #[serde(skip)]
    pub subdir: String,
    #[serde(skip)]
    pub file: String,
    #[serde(skip)]
    pub(crate) parsed: Option<Version>,
    #[serde(skip)]
    pub(crate) native: Option<Box<rattler_conda_types::RepoDataRecord>>,
}

impl Record {
    pub(crate) fn from_native(
        native: rattler_conda_types::RepoDataRecord,
        channel: String,
    ) -> Result<Self, serde_json::Error> {
        let mut record: Self =
            serde_json::from_value(serde_json::to_value(&native.package_record)?)?;
        record.channel = channel;
        record.subdir.clone_from(&native.package_record.subdir);
        record.file = native.identifier.to_file_name();
        record.parsed = Version::parse(&record.version);
        record.native = Some(Box::new(native));
        Ok(record)
    }

    /// The version, read once when the repodata was loaded.
    pub fn version(&self) -> Option<Version> {
        self.parsed
            .clone()
            .or_else(|| Version::parse(&self.version))
    }
}

/// The established repodata schema, shared by offline validation and loading.
pub(crate) type Repodata = rattler_conda_types::RepoData;

/// The packages of the channels of an environment for a platform, by name, each list in the
/// order of the channels, then of the newest first.
pub struct Index {
    pub(crate) channels: Vec<String>,
    by_name: HashMap<String, Vec<Record>>,
}

/// Where the repodata of a channel and platform is kept.
pub fn repodata_path(folder: &Path, channel: &str, subdir: &str) -> PathBuf {
    let channel = if channel.starts_with("https://") {
        Sha256::digest(crate::base(channel).as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    } else {
        channel.to_owned()
    };
    folder.join(channel).join(subdir).join("repodata.json")
}

pub(crate) fn component(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
}

/// Named channels or HTTPS channel URLs with no credentials, query or fragment.
pub fn valid_channel(channel: &str) -> bool {
    if channel.starts_with("https://") {
        reqwest::Url::parse(channel).is_ok_and(|url| {
            url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
        })
    } else {
        channel.split('/').all(component)
    }
}

/// The channels that a name of an environment file stands for: `defaults` is the main and R
/// channels of Anaconda, `nodefaults` stands for none.
pub fn channels(named: &[String]) -> Vec<String> {
    let mut channels = Vec::new();
    for channel in named {
        let expanded: &[&str] = match channel.as_str() {
            "nodefaults" => &[],
            "defaults" => &["pkgs/main", "pkgs/r"],
            other => &[other],
        };
        for channel in expanded {
            if !channels.iter().any(|known| known == channel) {
                channels.push((*channel).to_owned());
            }
        }
    }
    channels
}

impl Index {
    /// Reads the repodata of each channel for the platform and for `noarch`. Repodata that the
    /// folder lacks is an error that names every missing file. Channel and platform paths are
    /// lexically contained in the folder; the caller controls any symlinks below that folder.
    pub fn load(folder: &Path, channels: &[String], platform: &str) -> Result<Index, LockError> {
        if platform.parse::<rattler_conda_types::Subdir>().is_err()
            || channels.iter().any(|channel| !valid_channel(channel))
        {
            return Err(LockError::Io {
                path: folder.to_owned(),
                error: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid Conda channel or platform cache path",
                ),
            });
        }
        let mut by_name: HashMap<String, Vec<Record>> = HashMap::new();
        let mut missing = Vec::new();
        for channel in channels {
            for subdir in [platform, "noarch"] {
                let path = repodata_path(folder, channel, subdir);
                let text = match std::fs::read(&path) {
                    Ok(text) => text,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        missing.push((channel.clone(), subdir.to_owned(), path));
                        continue;
                    }
                    Err(error) => return Err(LockError::Io { path, error }),
                };
                let repodata: Repodata =
                    serde_json::from_slice(&text).map_err(|error| LockError::Repodata {
                        path: path.clone(),
                        error,
                    })?;
                let source = rattler_conda_types::Channel::from_str(
                    crate::base(channel),
                    &rattler_conda_types::ChannelConfig::default_with_root_dir(folder.to_owned()),
                )
                .map_err(|error| LockError::Io {
                    path: path.clone(),
                    error: io::Error::other(error),
                })?;
                for native in repodata.into_repo_data_records(&source) {
                    let record = Record::from_native(native, channel.clone()).map_err(|error| {
                        LockError::Repodata {
                            path: path.clone(),
                            error,
                        }
                    })?;
                    by_name.entry(record.name.clone()).or_default().push(record);
                }
            }
        }
        if !missing.is_empty() {
            return Err(LockError::MissingRepodata { missing });
        }
        let order = |channel: &str| channels.iter().position(|known| known == channel);
        for records in by_name.values_mut() {
            // A `.conda` file and a `.tar.bz2` file of the same package are one package.
            records.sort_by(|a, b| {
                order(&a.channel)
                    .cmp(&order(&b.channel))
                    .then_with(|| b.parsed.cmp(&a.parsed))
                    .then_with(|| b.build_number.cmp(&a.build_number))
                    .then_with(|| b.timestamp.cmp(&a.timestamp))
                    .then_with(|| b.file.ends_with(".conda").cmp(&a.file.ends_with(".conda")))
            });
            records.dedup_by(|a, b| {
                a.channel == b.channel && a.version == b.version && a.build == b.build
            });
        }
        Ok(Index {
            channels: channels.to_vec(),
            by_name,
        })
    }

    pub(crate) fn available(&self) -> Vec<Vec<&rattler_conda_types::RepoDataRecord>> {
        self.channels
            .iter()
            .map(|channel| {
                let mut records: Vec<_> = self
                    .by_name
                    .values()
                    .flatten()
                    .filter(|record| &record.channel == channel)
                    .filter_map(|record| record.native.as_deref())
                    .collect();
                records.sort_by(|a, b| a.identifier.cmp(&b.identifier));
                records
            })
            .collect()
    }
}
