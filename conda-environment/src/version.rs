//! Conda version ordering and constraints backed by rattler.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;

use rattler_conda_types::ParseStrictness;

/// A Conda version, compared using Conda's version ordering.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub(crate) rattler_conda_types::Version);

impl Version {
    pub fn parse(text: &str) -> Option<Self> {
        text.trim().parse().ok().map(Self)
    }

    pub fn starts_with(&self, prefix: &Version) -> bool {
        self.0.starts_with(&prefix.0)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// The version constraint of a match spec.
#[derive(Clone, Debug, PartialEq)]
pub struct VersionSpec(pub(crate) rattler_conda_types::VersionSpec);

impl VersionSpec {
    pub fn any() -> Self {
        Self(rattler_conda_types::VersionSpec::Any)
    }

    /// Bare versions are exact for package dependencies and prefixes for environment pins.
    pub fn parse(text: &str, exact: bool) -> Option<Self> {
        let text = text.trim();
        let text = text
            .strip_prefix("==")
            .filter(|rest| rest.contains('*'))
            .unwrap_or(text);
        if text.is_empty() {
            return Some(Self::any());
        }
        let prefix;
        let text = if !exact && Version::parse(text).is_some() {
            prefix = format!("{text}.*");
            &prefix
        } else {
            text
        };
        rattler_conda_types::VersionSpec::from_str(text, ParseStrictness::Lenient)
            .ok()
            .map(Self)
    }

    pub fn matches(&self, version: &Version) -> bool {
        self.0.matches(&version.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap_or_else(|| panic!("a version: {text}"))
    }

    /// The order of the conda documentation (conda.models.version), where each version is at
    /// most the next one; `0.4.1.rc` and `0.4.1.RC` are equal, as case does not count.
    #[test]
    fn versions_follow_the_order_of_conda() {
        let ordered = [
            "0.4",
            "0.4.0",
            "0.4.1.rc",
            "0.4.1.RC",
            "0.4.1",
            "0.4.2",
            "0.5a1",
            "0.5b3",
            "0.5C1",
            "0.5",
            "0.9.6",
            "1.0",
            "1.1dev1",
            "1.1_",
            "1.1a1",
            "1.1.0dev1",
            "1.1.a1",
            "1.1.0rc1",
            "1.1.0",
            "1.1.0post1",
            "1.1post1",
            "1996.07.12",
            "1!0.4.1",
            "1!3.1.1.6",
            "2!0.4.1",
        ];
        for pair in ordered.windows(2) {
            assert!(
                version(pair[0]) <= version(pair[1]),
                "{} <= {}",
                pair[0],
                pair[1]
            );
        }
        assert_eq!(version("0.4"), version("0.4.0"));
        assert_eq!(version("0.4.1.rc"), version("0.4.1.RC"));
        assert!(version("0.5a1") < version("0.5"));
        assert!(version("1.1post1") > version("1.1.0"));
        assert!(version("1.10") > version("1.9"));
        assert!(Version::parse("1.2 3").is_none());
    }

    #[test]
    fn specs_allow_ranges_prefixes_and_alternatives() {
        let spec = |text: &str, exact| VersionSpec::parse(text, exact).expect("a spec");
        assert!(spec(">=1.2,<2", false).matches(&version("1.9.9")));
        assert!(!spec(">=1.2,<2", false).matches(&version("2.0")));
        assert!(spec("1.8.*", false).matches(&version("1.8.3")));
        assert!(!spec("1.8.*", false).matches(&version("1.80")));
        assert!(spec("1.8", false).matches(&version("1.8.1")));
        assert!(!spec("1.8", true).matches(&version("1.8.1")));
        assert!(spec("1.8", true).matches(&version("1.8.0")));
        assert!(spec("1.1|1.3", true).matches(&version("1.3")));
        assert!(spec("~=1.4.2", false).matches(&version("1.4.9")));
        assert!(!spec("~=1.4.2", false).matches(&version("1.5")));
        assert!(spec("!=1.4", false).matches(&version("1.5")));
        assert!(!spec("!=1.2.*", false).matches(&version("1.2")));
        assert!(!spec("!=1.2.*", false).matches(&version("1.2.3")));
        assert!(spec("!=1.2.*", false).matches(&version("1.20")));
        assert!(!spec("!=1.2*", false).matches(&version("1.2.3")));
        assert!(spec(">=1,!=1.2.*", false).matches(&version("1.3")));
        assert!(spec("==3.9.*", false).matches(&version("3.9.18")));
        assert!(spec("=3.9", false).matches(&version("3.9.18")));
        assert!(spec("*", true).matches(&version("0")));
        assert!(VersionSpec::parse(">=x y", false).is_none());
    }
}
