//! Describes target Python markers and compatible wheel tags without probing the host.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use pep440_rs::Version;
use pep508_rs::{MarkerEnvironment, MarkerEnvironmentBuilder, Requirement, VersionOrUrl};

use crate::{Lock, PipError};

pub(crate) struct Target {
    pub python: Version,
    pub markers: MarkerEnvironment,
    pub machine: &'static str,
    glibc: Option<(u64, u64)>,
    musl: Option<(u64, u64)>,
}

impl Target {
    pub fn new(lock: &Lock) -> Result<Self, PipError> {
        let python = lock
            .packages
            .iter()
            .find(|package| package.name == "python")
            .ok_or_else(|| unsupported("pip requires a locked Conda Python and pip"))?;
        if !lock.packages.iter().any(|package| package.name == "pip") {
            return Err(unsupported(
                "pip sections require pip in the Conda dependencies",
            ));
        }
        if python.build.contains("pypy")
            || lock.packages.iter().any(|package| {
                package.name == "python-freethreading"
                    || (package.name == "python_abi" && package.build.ends_with('t'))
            })
        {
            return Err(unsupported("pip locking supports standard CPython only"));
        }
        let python: Version = python
            .version
            .parse()
            .map_err(|error| PipError::Metadata(format!("invalid Python: {error}")))?;
        let release = python.release();
        if release.len() < 2 || release[0] != 3 {
            return Err(unsupported("pip locking supports Python 3 only"));
        }
        let machine = match lock.platform.as_str() {
            "linux-64" => "x86_64",
            "linux-aarch64" => "aarch64",
            "linux-ppc64le" => "ppc64le",
            "linux-s390x" => "s390x",
            "linux-32" => "i686",
            _ => {
                return Err(unsupported(
                    "pip locking supports Linux container targets only",
                ));
            }
        };
        let full = python.to_string();
        let short = format!("{}.{}", release[0], release[1]);
        let markers = MarkerEnvironment::try_from(MarkerEnvironmentBuilder {
            implementation_name: "cpython",
            implementation_version: &full,
            os_name: "posix",
            platform_machine: machine,
            platform_python_implementation: "CPython",
            platform_release: "",
            platform_system: "Linux",
            platform_version: "",
            python_full_version: &full,
            python_version: &short,
            sys_platform: "linux",
        })
        .map_err(|error| PipError::Metadata(error.to_string()))?;
        let capability = |name| {
            lock.virtual_packages
                .as_deref()
                .unwrap_or_default()
                .iter()
                .find(|package| package.name == name)
                .and_then(|package| package.version.parse::<Version>().ok())
                .and_then(|version| {
                    version
                        .release()
                        .get(1)
                        .map(|minor| (version.release()[0], *minor))
                })
        };
        Ok(Self {
            python,
            markers,
            machine,
            glibc: capability("__glibc"),
            musl: capability("__musl"),
        })
    }

    pub fn wheel(&self, file: &str) -> Option<Version> {
        let parts: Vec<_> = file.strip_suffix(".whl")?.split('-').collect();
        if !(5..=6).contains(&parts.len()) {
            return None;
        }
        let tags = &parts[parts.len() - 3..];
        let minor = self.python.release()[1];
        let cpython = format!("cp3{minor}");
        let generic = format!("py3{minor}");
        let compatible = tags[0].split('.').any(|tag| {
            tags[1].split('.').any(|abi| {
                (tag == cpython && (abi == cpython || abi == "none" || abi == "abi3"))
                    || ((tag == "py3" || tag == generic) && abi == "none")
                    || (abi == "abi3"
                        && tag
                            .strip_prefix("cp3")
                            .and_then(|value| value.parse::<u64>().ok())
                            .is_some_and(|value| value >= 2 && value <= minor))
            })
        });
        if !compatible || !tags[2].split('.').any(|tag| self.platform(tag)) {
            return None;
        }
        parts[1].parse().ok()
    }

    fn platform(&self, tag: &str) -> bool {
        if tag == "any" || tag == format!("linux_{}", self.machine) {
            return true;
        }
        let Some(prefix) = tag.strip_suffix(&format!("_{}", self.machine)) else {
            return false;
        };
        let (needed, available) = if let Some(value) = prefix.strip_prefix("manylinux_") {
            (pair(value), self.glibc)
        } else if let Some(value) = prefix.strip_prefix("musllinux_") {
            (pair(value), self.musl)
        } else {
            (
                match prefix {
                    "manylinux1" => Some((2, 5)),
                    "manylinux2010" => Some((2, 12)),
                    "manylinux2014" => Some((2, 17)),
                    _ => None,
                },
                self.glibc,
            )
        };
        needed
            .zip(available)
            .is_some_and(|(needed, available)| needed.0 == available.0 && needed.1 <= available.1)
    }
}

fn pair(text: &str) -> Option<(u64, u64)> {
    let (major, minor) = text.split_once('_')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

pub(crate) fn requirement(text: &str) -> Result<Requirement, PipError> {
    if text.trim().starts_with('-')
        || text.contains(['\n', '\r'])
        || [".whl", ".tar.gz", ".zip"]
            .iter()
            .any(|suffix| text.trim().ends_with(suffix))
    {
        return Err(unsupported(
            "pip options, editable installs and local archives are unsupported",
        ));
    }
    let requirement: Requirement = text
        .parse()
        .map_err(|_| unsupported("invalid pip requirement (contents redacted)"))?;
    if matches!(requirement.version_or_url, Some(VersionOrUrl::Url(_))) {
        return Err(unsupported(
            "pip URLs, local paths and VCS requirements are unsupported",
        ));
    }
    let marker = requirement
        .marker
        .contents()
        .map(|value| value.to_string())
        .unwrap_or_default();
    if marker.contains("platform_release") || marker.contains("platform_version") {
        return Err(unsupported(format!(
            "pip kernel markers need unavailable target metadata: {text}"
        )));
    }
    Ok(requirement)
}

pub(crate) fn unsupported(message: impl Into<String>) -> PipError {
    PipError::Unsupported(message.into())
}
