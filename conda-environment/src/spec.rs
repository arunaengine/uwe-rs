//! Match specs: what an environment or a package asks for, like `samtools=1.17`,
//! `bioconda::fastqc=0.12.1=hdfd78af_0`, `python >=3.8,<3.9.0a0` or `numpy 1.21.* py39*`.
//!
//! In the form of environment files, `name=1.8` asks for a version that starts with `1.8`,
//! `name==1.8` for exactly `1.8`, and `name=1.8=build` for exactly that version and build. In
//! the form of the dependencies of packages, `name 1.8` asks for exactly `1.8`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;

use crate::Record;
use crate::version::{Version, VersionSpec};

/// What a match spec asks for.
#[derive(Clone, Debug, PartialEq)]
pub struct MatchSpec {
    /// The channel that must have the package, like `bioconda` of `bioconda::fastqc`.
    pub channel: Option<String>,
    pub name: String,
    pub version: VersionSpec,
    /// A pattern of the build string, where `*` stands for any text.
    pub build: Option<String>,
    text: String,
}

/// A match spec that cannot be read.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("`{text}` is no match spec of a conda package: {reason}")]
pub struct SpecError {
    pub text: String,
    pub reason: &'static str,
}

impl MatchSpec {
    pub fn parse(text: &str) -> Result<MatchSpec, SpecError> {
        let failed = |reason| SpecError {
            text: text.to_owned(),
            reason,
        };
        let trimmed = text.split('#').next().unwrap_or_default().trim();
        if trimmed.contains('[') {
            return Err(failed("the form with brackets is not supported yet"));
        }
        let (channel, rest) = match trimmed.split_once("::") {
            // A channel may name its platform too, like `conda-forge/linux-64::zlib`.
            Some((channel, rest)) => {
                let channel = channel.split('/').next().unwrap_or(channel);
                (Some(channel.to_owned()), rest)
            }
            None => (None, trimmed),
        };
        // As conda does, spaces around operators and commas count for nothing, so that
        // `bcftools >= 1.19, <2` is `bcftools>=1.19,<2`.
        let rest = tight(rest);
        let end = rest
            .find(|c: char| !name_character(c))
            .unwrap_or(rest.len());
        let (name, rest) = rest.split_at(end);
        if name.is_empty() {
            return Err(failed("its name"));
        }
        let rest = rest.trim();
        let (version, build) = match rest.chars().next() {
            None => (VersionSpec::any(), None),
            // The form of environment files: `=1.8`, `==1.8`, `=1.8=build`, `>=1.2,<2`,
            // with a build after a space too.
            Some('=' | '<' | '>' | '!' | '~') => {
                let (version, build) = match rest.split_once(char::is_whitespace) {
                    Some((version, build)) => (version, Some(build.trim())),
                    None => (rest, None),
                };
                operator_form(version, build).ok_or_else(|| failed("its version"))?
            }
            // The form of the dependencies of packages: `name 1.8 build`.
            Some(first) if first.is_whitespace() || first.is_ascii_alphanumeric() => {
                let mut words = rest.split_whitespace();
                let version = words.next().unwrap_or_default();
                let build = words.next();
                if words.next().is_some() {
                    return Err(failed("it has more than a name, a version and a build"));
                }
                let spec =
                    VersionSpec::parse(version, true).ok_or_else(|| failed("its version"))?;
                (spec, build)
            }
            Some(_) => return Err(failed("its name")),
        };
        Ok(MatchSpec {
            channel,
            name: name.to_ascii_lowercase(),
            version,
            build: build.filter(|build| *build != "*").map(str::to_owned),
            text: trimmed.to_owned(),
        })
    }

    /// Whether a package has the name, version and build that this spec asks for. The channel
    /// is the resolver's to check.
    pub fn matches(&self, record: &Record) -> bool {
        let version = match &record.parsed {
            Some(version) => self.version.matches(version),
            None => Version::parse(&record.version).is_some_and(|v| self.version.matches(&v)),
        };
        record.name == self.name
            && version
            && (self.build.as_deref()).is_none_or(|pattern| glob(pattern, &record.build))
    }
}

impl fmt::Display for MatchSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// Whether a character belongs to the name of a package.
fn name_character(c: char) -> bool {
    c.is_ascii_alphanumeric() || "_.-".contains(c)
}

/// The text without spaces around the characters of operators and commas.
fn tight(text: &str) -> String {
    let operator = |c: char| "=<>!~,|".contains(c);
    let characters: Vec<char> = text.trim().chars().collect();
    let mut tight = String::with_capacity(text.len());
    for (index, c) in characters.iter().enumerate() {
        if c.is_whitespace() {
            let before = tight.chars().last().is_some_and(operator);
            let after = (characters[index + 1..].iter())
                .find(|next| !next.is_whitespace())
                .is_some_and(|next| operator(*next));
            if before || after {
                continue;
            }
        }
        tight.push(*c);
    }
    tight
}

/// The version and build of the form of environment files, after the name: `=1.8` for the
/// versions that start with it, `==1.8` for exactly it, `=1.8=build` for exactly that version
/// and build, and constraints like `>=1.2,<2`; a build may follow after a space.
fn operator_form<'a>(
    version: &'a str,
    build: Option<&'a str>,
) -> Option<(VersionSpec, Option<&'a str>)> {
    let build = build.map(|build| build.trim_start_matches('='));
    if let Some(exact) = version.strip_prefix("==") {
        return Some((VersionSpec::parse(exact, true)?, build));
    }
    let Some(rest) = version.strip_prefix('=') else {
        return Some((VersionSpec::parse(version, false)?, build));
    };
    match rest.split_once('=') {
        // With a build, the version is exact.
        Some((version, own)) => Some((VersionSpec::parse(version, true)?, Some(own))),
        None => Some((VersionSpec::parse(rest, build.is_some())?, build)),
    }
}

/// Whether a text matches a pattern in which `*` stands for any text.
pub(crate) fn glob(pattern: &str, text: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    let Some((last, middle)) = parts.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}
