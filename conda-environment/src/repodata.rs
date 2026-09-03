//! The packages of channels, from their repodata in a local folder: for each channel and
//! platform `<folder>/<channel>/<platform>/repodata.json`, as conda serves it at
//! `https://conda.anaconda.org/<channel>/<platform>/repodata.json`. Nothing is downloaded here.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
}

impl Record {
    /// The version, read once when the repodata was loaded.
    pub fn version(&self) -> Option<Version> {
        self.parsed
            .clone()
            .or_else(|| Version::parse(&self.version))
    }
}

/// The repodata file of a channel and platform.
#[derive(Deserialize)]
pub(crate) struct Repodata {
    #[serde(default)]
    packages: BTreeMap<String, Record>,
    #[serde(default, rename = "packages.conda")]
    conda_packages: BTreeMap<String, Record>,
}

/// The packages of the channels of an environment for a platform, by name, each list in the
/// order of the channels, then of the newest first.
pub struct Index {
    pub(crate) channels: Vec<String>,
    by_name: HashMap<String, Vec<Record>>,
}

/// Where the repodata of a channel and platform is kept.
pub fn repodata_path(folder: &Path, channel: &str, subdir: &str) -> PathBuf {
    folder.join(channel).join(subdir).join("repodata.json")
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
    /// folder lacks is an error that names every missing file.
    pub fn load(folder: &Path, channels: &[String], platform: &str) -> Result<Index, LockError> {
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
                let repodata: Repodata = serde_json::from_slice(&text)
                    .map_err(|error| LockError::Repodata { path, error })?;
                let files = repodata.packages.into_iter().chain(repodata.conda_packages);
                for (file, mut record) in files {
                    record.parsed = Version::parse(&record.version);
                    record.channel = channel.clone();
                    record.subdir = subdir.to_owned();
                    record.file = file;
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

    /// The packages of a name, from the first channel that has it, or from the channel named.
    pub(crate) fn candidates(&self, name: &str, channel: Option<&str>) -> Vec<&Record> {
        let Some(records) = self.by_name.get(name) else {
            return Vec::new();
        };
        let wanted = channel.or_else(|| records.first().map(|record| record.channel.as_str()));
        (records.iter())
            .filter(|record| Some(record.channel.as_str()) == wanted)
            .collect()
    }
}
