//! Locks package addresses and digests for reproducible environments and container images.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::repodata::Record;

/// The version of the format of locks; a change of the format raises it.
pub const FORMAT: u32 = 1;

/// A capability of the target container, supplied without probing the driver host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualPackage {
    pub name: String,
    pub version: String,
    pub build: String,
}

/// The packages of an environment for a platform.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lock {
    pub format: u32,
    /// The platform, like `linux-64`.
    pub platform: String,
    pub channels: Vec<String>,
    /// The match specs of the environment file.
    pub specs: Vec<String>,
    /// The original pip requirements from the environment file.
    pub pip: Vec<String>,
    /// The packages, sorted by name.
    pub packages: Vec<Package>,
    /// Target capabilities, or `None` when compatibility is unknown in a legacy lock.
    /// An empty list explicitly means that no virtual packages are present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub virtual_packages: Option<Vec<VirtualPackage>>,
    /// Exact pip artifacts, absent in historical and Conda-only locks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pypi: Option<PipLock>,
}

/// Versioned pip resolution for the lock's platform and exact Conda Python.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipLock {
    pub format: u32,
    pub python: String,
    pub packages: Vec<PipPackage>,
}

/// One hash-pinned wheel or source distribution, sorted by normalized project name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipPackage {
    pub name: String,
    pub version: String,
    pub file: String,
    pub url: String,
    pub sha256: String,
}

/// A locked package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub build: String,
    pub channel: String,
    pub subdir: String,
    /// The file of the package, like `samtools-1.17-hd87286a_1.tar.bz2`.
    pub file: String,
    pub url: String,
    pub sha256: Option<String>,
    pub md5: Option<String>,
}

impl Package {
    pub(crate) fn of(record: &Record) -> Package {
        Package {
            name: record.name.clone(),
            version: record.version.clone(),
            build: record.build.clone(),
            channel: record.channel.clone(),
            subdir: record.subdir.clone(),
            file: record.file.clone(),
            url: record
                .native
                .as_ref()
                .map(|native| native.url.to_string())
                .unwrap_or_else(|| {
                    format!(
                        "{}/{}/{}",
                        base(&record.channel),
                        record.subdir,
                        record.file
                    )
                }),
            sha256: record.sha256.clone(),
            md5: record.md5.clone(),
        }
    }
}

/// Where a channel serves its packages: Anaconda's own channels at repo.anaconda.com, the
/// others at conda.anaconda.org.
pub fn base(channel: &str) -> String {
    if let Ok(url) = reqwest::Url::parse(channel)
        && url.scheme() == "https"
    {
        return url.as_str().trim_end_matches('/').to_owned();
    }
    match channel.strip_prefix("pkgs/") {
        Some(_) => format!("https://repo.anaconda.com/{channel}"),
        None => format!("https://conda.anaconda.org/{channel}"),
    }
}

impl Lock {
    /// Checks versioned pip pins before acquisition or image preparation.
    pub fn pip_packages(&self) -> std::io::Result<&[PipPackage]> {
        let invalid = |message| std::io::Error::new(std::io::ErrorKind::InvalidData, message);
        let Some(pypi) = &self.pypi else {
            return if self.pip.is_empty() {
                Ok(&[])
            } else {
                Err(invalid(
                    "pip requirements need a versioned hash-pinned lock",
                ))
            };
        };
        let target = crate::pypi::Target::new(self).map_err(std::io::Error::other)?;
        if pypi.format != 1 || pypi.python != target.python.to_string() {
            return Err(invalid("unsupported pip lock format or mismatched Python"));
        }
        for (index, package) in pypi.packages.iter().enumerate() {
            let version = package.version.parse::<pep440_rs::Version>().ok();
            let name = package.name.parse::<pep508_rs::PackageName>().ok();
            let source = package.file.ends_with(".tar.gz") || package.file.ends_with(".zip");
            if !crate::distribution::valid_hash(&package.sha256)
                || !crate::distribution::safe_file(&package.file)
                || !reqwest::Url::parse(&package.url)
                    .is_ok_and(|url| crate::distribution::safe_url(&url))
                || version.is_none()
                || name.as_ref().map(|name| name.as_ref()) != Some(package.name.as_str())
                || (!source && target.wheel(&package.file) != version)
                || (index > 0 && pypi.packages[index - 1].name >= package.name)
            {
                return Err(invalid(
                    "pip lock needs sorted unique compatible artifacts with HTTPS URLs and SHA-256",
                ));
            }
            if source
                && ["setuptools", "wheel"]
                    .iter()
                    .any(|name| !self.packages.iter().any(|package| package.name == *name))
            {
                return Err(invalid(
                    "pip source distribution requires locked setuptools and wheel build tools",
                ));
            }
        }
        Ok(&pypi.packages)
    }

    /// `sha256:<hex>` of the lock as JSON, which names the environment.
    pub fn digest(&self) -> String {
        // Serializing a lock cannot fail: its fields are texts and lists.
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        let hex: String = (Sha256::digest(&bytes).iter())
            .map(|byte| format!("{byte:02x}"))
            .collect();
        format!("sha256:{hex}")
    }

    /// The explicit list that conda and micromamba install from without solving:
    /// `@EXPLICIT`, then the address of each package with its MD5 digest.
    /// Pip packages require the separate hash-pinned installation used by image preparation.
    pub fn explicit(&self) -> String {
        let mut text = format!("# platform: {}\n@EXPLICIT\n", self.platform);
        for package in &self.packages {
            text.push_str(&package.url);
            if let Some(md5) = &package.md5 {
                text.push('#');
                text.push_str(md5);
            }
            text.push('\n');
        }
        text
    }
}
