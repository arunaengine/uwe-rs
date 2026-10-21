//! Reads hash-verified wheel metadata and static source distributions for native locking.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::io::{self, Cursor, Read};

use pep440_rs::{Version, VersionSpecifiers};
use pep508_rs::{ExtraName, Requirement, VersionOrUrl};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use crate::pypi::{Target, requirement, unsupported};
use crate::{Lock, PipError, PipPackage};

#[derive(Deserialize)]
pub(crate) struct Artifact {
    filename: String,
    url: String,
    hashes: BTreeMap<String, String>,
    #[serde(default, rename = "requires-python")]
    python: Option<String>,
    #[serde(default)]
    yanked: serde_json::Value,
    #[serde(default, rename = "core-metadata")]
    metadata: serde_json::Value,
    #[serde(default, rename = "dist-info-metadata")]
    legacy_metadata: serde_json::Value,
}

#[derive(Clone)]
pub(crate) struct Candidate {
    pub package: PipPackage,
    pub version: Version,
    metadata: Option<String>,
}

#[derive(Clone)]
pub(crate) struct Metadata {
    pub requires: Vec<Requirement>,
    pub extras: Vec<ExtraName>,
    pub python: Option<VersionSpecifiers>,
}

impl Artifact {
    pub fn candidate(self, name: &str, target: &Target) -> Result<Option<Candidate>, PipError> {
        if self.yanked != serde_json::Value::Null && self.yanked != false {
            return Ok(None);
        }
        if let Some(python) = &self.python {
            let specifiers: VersionSpecifiers = python
                .parse()
                .map_err(|error| PipError::Metadata(format!("invalid Requires-Python: {error}")))?;
            if !specifiers.contains(&target.python) {
                return Ok(None);
            }
        }
        let version = if self.filename.ends_with(".whl") {
            target.wheel(&self.filename)
        } else {
            self.filename
                .strip_suffix(".tar.gz")
                .or_else(|| self.filename.strip_suffix(".zip"))
                .and_then(|stem| stem.rsplit_once('-'))
                .and_then(|(_, version)| version.parse().ok())
        };
        let Some(version) = version else {
            return Ok(None);
        };
        let hash = self
            .hashes
            .get("sha256")
            .filter(|hash| valid_hash(hash))
            .ok_or_else(|| unsupported(format!("{} has no valid SHA-256", self.filename)))?;
        let url = reqwest::Url::parse(&self.url)
            .map_err(|error| PipError::Metadata(error.to_string()))?;
        if !safe_url(&url) || !safe_file(&self.filename) {
            return Err(unsupported(format!(
                "invalid HTTPS pip artifact: {}",
                self.filename
            )));
        }
        Ok(Some(Candidate {
            package: PipPackage {
                name: name.into(),
                version: version.to_string(),
                file: self.filename,
                url: self.url,
                sha256: hash.to_ascii_lowercase(),
            },
            version,
            metadata: self
                .metadata
                .get("sha256")
                .or_else(|| self.legacy_metadata.get("sha256"))
                .and_then(serde_json::Value::as_str)
                .filter(|hash| valid_hash(hash))
                .map(str::to_owned),
        }))
    }
}

pub(crate) fn metadata(
    candidate: &Candidate,
    lock: &Lock,
    fetch: &mut impl FnMut(&str) -> io::Result<Vec<u8>>,
) -> Result<Metadata, PipError> {
    let package = &candidate.package;
    let source = !package.file.ends_with(".whl");
    if source {
        for name in ["setuptools", "wheel"] {
            if !lock.packages.iter().any(|package| package.name == name) {
                return Err(unsupported(format!(
                    "{} needs source build tools: add setuptools and wheel to the Conda environment",
                    package.name
                )));
            }
        }
    }
    let (bytes, build) = if !source && let Some(hash) = &candidate.metadata {
        let bytes = fetch(&format!("{}.metadata", package.url))?;
        verify(&bytes, hash)?;
        (bytes, None)
    } else {
        let bytes = fetch(&package.url)?;
        verify(&bytes, &package.sha256)?;
        archive_metadata(&bytes, &package.file)?
    };
    let fields = fields(&bytes)?;
    for name in ["name", "version", "requires-python", "metadata-version"] {
        if fields.get(name).is_some_and(|values| values.len() > 1) {
            return Err(PipError::Metadata(format!("duplicate {name} metadata")));
        }
    }
    if fields
        .get("name")
        .and_then(|values| values.first())
        .and_then(|name| name.parse::<pep508_rs::PackageName>().ok())
        .as_ref()
        .map(|name| name.as_ref())
        != Some(package.name.as_str())
        || fields
            .get("version")
            .and_then(|values| values.first())
            .and_then(|version| version.parse::<Version>().ok())
            .as_ref()
            != Some(&candidate.version)
    {
        return Err(PipError::Metadata(format!(
            "{} metadata name or version differs from artifact",
            package.file
        )));
    }
    if source {
        let version = fields
            .get("metadata-version")
            .and_then(|values| values.first())
            .map(String::as_str)
            .unwrap_or("");
        if version
            .parse::<Version>()
            .ok()
            .is_none_or(|version| version < "2.2".parse().expect("fixed version"))
            || fields.get("dynamic").is_some_and(|values| {
                values.iter().any(|value| {
                    [
                        "requires-dist",
                        "requires-python",
                        "provides-extra",
                        "name",
                        "version",
                    ]
                    .contains(&value.to_ascii_lowercase().as_str())
                })
            })
        {
            return Err(unsupported(format!(
                "{} needs dynamic source metadata; native locking requires static Metadata-Version >=2.2",
                package.name
            )));
        }
        if let Some(build) = build {
            check_build(&build, lock)?;
        }
    }
    let requires = fields
        .get("requires-dist")
        .into_iter()
        .flatten()
        .map(|text| requirement(text))
        .collect::<Result<_, _>>()?;
    let extras = fields
        .get("provides-extra")
        .into_iter()
        .flatten()
        .map(|text| {
            text.parse()
                .map_err(|error| PipError::Metadata(format!("invalid extra: {error}")))
        })
        .collect::<Result<_, _>>()?;
    let python = fields
        .get("requires-python")
        .and_then(|values| values.first())
        .map(|text| {
            text.parse()
                .map_err(|error| PipError::Metadata(format!("invalid Requires-Python: {error}")))
        })
        .transpose()?;
    Ok(Metadata {
        requires,
        extras,
        python,
    })
}

fn archive_metadata(bytes: &[u8], file: &str) -> Result<(Vec<u8>, Option<Vec<u8>>), PipError> {
    let mut metadata = None;
    let mut build = None;
    if file.ends_with(".tar.gz") {
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes).take(256 << 20));
        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.into_owned();
            let parts: Vec<_> = path.components().collect();
            if parts.len() == 2 && path.ends_with("PKG-INFO") {
                if metadata.is_some() {
                    return Err(PipError::Metadata(
                        "multiple distribution metadata entries".into(),
                    ));
                }
                metadata = Some(read_metadata(&mut entry)?);
            } else if parts.len() == 2 && path.ends_with("pyproject.toml") {
                build = Some(read_metadata(&mut entry)?);
            }
        }
    } else {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|error| PipError::Metadata(error.to_string()))?;
        for index in 0..archive.len() {
            let mut entry = archive
                .by_index(index)
                .map_err(|error| PipError::Metadata(error.to_string()))?;
            let name = entry
                .name()
                .map_err(|error| PipError::Metadata(error.to_string()))?
                .into_owned();
            let parts: Vec<_> = name.split('/').collect();
            if parts.len() == 2
                && ((file.ends_with(".whl")
                    && parts[0].ends_with(".dist-info")
                    && parts[1] == "METADATA")
                    || (file.ends_with(".zip") && parts[1] == "PKG-INFO"))
            {
                if metadata.is_some() {
                    return Err(PipError::Metadata(
                        "multiple distribution metadata entries".into(),
                    ));
                }
                metadata = Some(read_metadata(&mut entry)?);
            } else if file.ends_with(".zip") && parts.len() == 2 && parts[1] == "pyproject.toml" {
                build = Some(read_metadata(&mut entry)?);
            }
        }
    }
    Ok((
        metadata
            .ok_or_else(|| unsupported(format!("{file} has no static distribution metadata")))?,
        build,
    ))
}

fn check_build(bytes: &[u8], lock: &Lock) -> Result<(), PipError> {
    let text = std::str::from_utf8(bytes).map_err(|error| PipError::Metadata(error.to_string()))?;
    let project: toml::Value =
        toml::from_str(text).map_err(|error| PipError::Metadata(error.to_string()))?;
    if project
        .get("build-system")
        .and_then(|table| table.get("backend-path"))
        .is_some()
    {
        return Err(unsupported("source backend-path is unsupported"));
    }
    let Some(requires) = project
        .get("build-system")
        .and_then(|table| table.get("requires"))
        .and_then(toml::Value::as_array)
    else {
        return Ok(());
    };
    for text in requires {
        let text = text
            .as_str()
            .ok_or_else(|| unsupported("invalid source build requirement"))?;
        let requirement = requirement(text)?;
        let package = lock
            .packages
            .iter()
            .find(|package| package.name.replace('_', "-") == requirement.name.as_ref())
            .ok_or_else(|| {
                unsupported(format!(
                    "source build requires {text} in the locked Conda environment"
                ))
            })?;
        let version: Version = package
            .version
            .parse()
            .map_err(|error| PipError::Metadata(format!("source build version: {error}")))?;
        if !requirement.extras.is_empty()
            || matches!(&requirement.version_or_url, Some(VersionOrUrl::VersionSpecifier(specifiers)) if !specifiers.contains(&version))
        {
            return Err(unsupported(format!(
                "locked Conda build tool does not satisfy {text}"
            )));
        }
    }
    Ok(())
}

fn read_metadata(reader: &mut impl Read) -> Result<Vec<u8>, PipError> {
    let mut bytes = Vec::new();
    reader.take((8 << 20) + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 8 << 20 {
        return Err(unsupported("distribution metadata exceeds 8 MiB"));
    }
    Ok(bytes)
}

fn fields(bytes: &[u8]) -> Result<BTreeMap<String, Vec<String>>, PipError> {
    let text = std::str::from_utf8(bytes).map_err(|error| PipError::Metadata(error.to_string()))?;
    let mut fields: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut previous = String::new();
    for line in text.lines() {
        if line.is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            let value = fields
                .get_mut(&previous)
                .and_then(|values| values.last_mut())
                .ok_or_else(|| PipError::Metadata("invalid folded metadata".into()))?;
            value.push(' ');
            value.push_str(line.trim());
        } else {
            let (key, value) = line
                .split_once(':')
                .ok_or_else(|| PipError::Metadata("invalid metadata header".into()))?;
            previous = key.to_ascii_lowercase();
            fields
                .entry(previous.clone())
                .or_default()
                .push(value.trim().to_owned());
        }
    }
    Ok(fields)
}

pub(crate) fn verify(bytes: &[u8], expected: &str) -> Result<(), PipError> {
    let actual: String = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(PipError::Metadata("artifact SHA-256 mismatch".into()));
    }
    Ok(())
}

pub(crate) fn valid_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(crate) fn safe_file(file: &str) -> bool {
    !file.is_empty()
        && file
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-+!".contains(&byte))
}

pub(crate) fn safe_url(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}
