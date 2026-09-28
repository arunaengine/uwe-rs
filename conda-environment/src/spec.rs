//! Conda match specs backed by rattler, including bracket constraints.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;

use rattler_conda_types::{Matches, PackageRecord, ParseMatchSpecOptions};

use crate::Record;
use crate::version::VersionSpec;

/// What a match spec asks for.
#[derive(Clone, Debug, PartialEq)]
pub struct MatchSpec {
    pub channel: Option<String>,
    pub name: String,
    pub version: VersionSpec,
    pub build: Option<String>,
    text: String,
    pub(crate) native: rattler_conda_types::MatchSpec,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("`{text}` is no match spec of a conda package: {reason}")]
pub struct SpecError {
    pub text: String,
    pub reason: &'static str,
}

impl MatchSpec {
    pub fn parse(text: &str) -> Result<Self, SpecError> {
        let failed = |reason| SpecError {
            text: text.to_owned(),
            reason,
        };
        let native =
            rattler_conda_types::MatchSpec::from_str(text, ParseMatchSpecOptions::lenient())
                .map_err(|_| failed("invalid Conda match spec"))?;
        if native.namespace.is_some() {
            return Err(failed("Conda namespaces are unsupported"));
        }
        let name = native.name.as_exact().ok_or_else(|| failed("its name"))?;
        Ok(Self {
            channel: native.channel.as_ref().map(|channel| {
                channel
                    .name
                    .clone()
                    .unwrap_or_else(|| channel.base_url.as_str().trim_end_matches('/').to_owned())
            }),
            name: name.as_normalized().to_owned(),
            version: VersionSpec(
                native
                    .version
                    .clone()
                    .unwrap_or(rattler_conda_types::VersionSpec::Any),
            ),
            build: native.build.as_ref().map(ToString::to_string),
            text: text.trim().to_owned(),
            native,
        })
    }

    pub fn matches(&self, record: &Record) -> bool {
        let (Some(version), Ok(name)) = (record.version(), record.name.parse()) else {
            return false;
        };
        let mut package = PackageRecord::new(name, version.0, record.build.clone());
        package.build_number = record.build_number;
        package.subdir.clone_from(&record.subdir);
        let mut spec = self.native.clone();
        let digest = |bytes: &[u8]| {
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        let hashes = spec
            .sha256
            .as_ref()
            .is_none_or(|hash| record.sha256.as_deref() == Some(digest(hash).as_str()))
            && spec
                .md5
                .as_ref()
                .is_none_or(|hash| record.md5.as_deref() == Some(digest(hash).as_str()));
        spec.sha256 = None;
        spec.md5 = None;
        hashes
            && spec.url.as_ref().is_none_or(|url| {
                record
                    .native
                    .as_ref()
                    .is_some_and(|record| &record.url == url)
            })
            && spec.matches(
                record
                    .native
                    .as_ref()
                    .map(|record| &record.package_record)
                    .unwrap_or(&package),
            )
            && spec.channel.as_ref().is_none_or(|channel| {
                channel.base_url.as_str().trim_end_matches('/') == crate::base(&record.channel)
            })
            && spec
                .subdir
                .as_ref()
                .is_none_or(|subdir| subdir == &record.subdir)
            && spec
                .file_name
                .as_ref()
                .is_none_or(|file| file == &record.file)
    }
}

impl fmt::Display for MatchSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &str, version: &str, build: &str) -> Record {
        Record {
            channel: "bioconda".into(),
            name: name.into(),
            version: version.into(),
            build: build.into(),
            ..Record::default()
        }
    }

    fn spec(text: &str) -> MatchSpec {
        MatchSpec::parse(text).unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn parses_environment_forms() {
        let fastqc = spec("bioconda::fastqc=0.12.1=hdfd78af_0");
        assert_eq!(fastqc.channel.as_deref(), Some("bioconda"));
        assert!(fastqc.matches(&record("fastqc", "0.12.1", "hdfd78af_0")));
        assert!(!fastqc.matches(&record("fastqc", "0.12.1", "hdfd78af_1")));
        let samtools = spec("samtools=1.17");
        assert!(samtools.matches(&record("samtools", "1.17", "h00cdaf9_0")));
        assert!(samtools.matches(&record("samtools", "1.17.1", "h00cdaf9_0")));
        assert!(!samtools.matches(&record("samtools", "1.18", "h00cdaf9_0")));
        assert!(!spec("samtools==1.17").matches(&record("samtools", "1.17.1", "0")));
        assert!(spec("r-base>=4.1,<4.3").matches(&record("r-base", "4.2.3", "0")));
        assert!(spec("R-Base").matches(&record("r-base", "4.2.3", "0")));
        assert!(spec("python=3.10 # a comment").matches(&record("python", "3.10.8", "0")));
        // Spaces around operators and commas count for nothing.
        assert!(spec("bcftools = 1.15").matches(&record("bcftools", "1.15.1", "0")));
        assert!(!spec("annotsv == 3.4.2").matches(&record("annotsv", "3.4.3", "0")));
        assert!(spec("bbmap >= 39.01, <40").matches(&record("bbmap", "39.06", "0")));
        assert!(!spec("bbmap >= 39.01, <40").matches(&record("bbmap", "40.0", "0")));
    }

    #[test]
    fn parses_dependency_specs() {
        let python = spec("python >=3.8,<3.9.0a0");
        assert!(python.matches(&record("python", "3.8.18", "h955ad1f_0")));
        assert!(!python.matches(&record("python", "3.9.0", "0")));
        let abi = spec("python_abi 3.8.* *_cp38");
        assert!(abi.matches(&record("python_abi", "3.8", "4_cp38")));
        assert!(!abi.matches(&record("python_abi", "3.8", "4_cp39")));
        assert!(spec("libzlib 1.2.13").matches(&record("libzlib", "1.2.13", "0")));
        let cpython = spec("python >=3.8 *_cpython");
        assert!(cpython.matches(&record("python", "3.12.1", "h1_0_cpython")));
        assert!(!cpython.matches(&record("python", "3.12.1", "h1_0_pypy")));
        assert!(!spec("libzlib 1.2.13").matches(&record("libzlib", "1.2.13.1", "0")));
    }

    #[test]
    fn preserves_bracket_constraints() {
        let numpy = spec("numpy[version='>=1.20',build=py*]");
        assert!(numpy.matches(&record("numpy", "1.21", "py39_0")));
        assert!(!numpy.matches(&record("numpy", "1.19", "py39_0")));
        assert!(!numpy.matches(&record("numpy", "1.21", "other")));
        let zlib = spec("conda-forge/linux-64::zlib");
        assert_eq!(zlib.channel.as_deref(), Some("conda-forge"));
        assert_eq!(zlib.native.subdir.as_deref(), Some("linux-64"));
        assert!(MatchSpec::parse("https://example.org/custom/linux-64::zlib").is_ok());
    }

    #[test]
    fn rejects_invalid_specs() {
        for text in ["", "x=>=<1", "b:c=1", "numpy[unknown=1]"] {
            assert!(MatchSpec::parse(text).is_err(), "{text}");
        }
    }
}
