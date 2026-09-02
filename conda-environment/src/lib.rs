//! Conda environments, as far as they work without network (decision 12): environment files
//! and their match specs, the order of conda versions, and the lock of an environment, which a
//! small resolver makes from the repodata of its channels in a local folder. The lock names
//! the packages by address and digest, so that an image built from it is the same anywhere.
//! Building the image, and downloading repodata and packages, need the network; see
//! docs/design/conda.md.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod environment;
mod lock;
mod repodata;
mod solve;
mod spec;
mod version;

use std::path::{Path, PathBuf};

pub use environment::{Environment, EnvironmentError};
pub use lock::{FORMAT, Lock, Package, VirtualPackage, base};
pub use repodata::{Index, Record, channels, repodata_path};
pub use spec::{MatchSpec, SpecError};
pub use version::{Version, VersionSpec};

/// Why an environment cannot be locked.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error(
        "the repodata of {} is not in the folder of repodata; locking needs it, and getting it \
         needs the network: {}",
        missing_names(missing),
        missing_downloads(missing)
    )]
    MissingRepodata {
        /// Each channel and platform, with where its repodata belongs.
        missing: Vec<(String, String, PathBuf)>,
    },
    #[error("{path}: {error}")]
    Io {
        path: PathBuf,
        error: std::io::Error,
    },
    #[error("{path} is no repodata: {error}")]
    Repodata {
        path: PathBuf,
        error: serde_json::Error,
    },
    #[error("no package of the channels {} fits `{spec}`", channels.join(", "))]
    NoPackage { spec: String, channels: Vec<String> },
    #[error("the packages of `{}` cannot all be installed together", specs.join("`, `"))]
    Conflict { specs: Vec<String> },
    #[error(
        "the environment needs a longer search than the {tries} tries of the resolver of iunctus"
    )]
    TooComplex { tries: usize },
    #[error("the package {package} names a dependency that cannot be read: {error}")]
    Package { package: String, error: SpecError },
    #[error(transparent)]
    Spec(SpecError),
}

fn missing_names(missing: &[(String, String, PathBuf)]) -> String {
    let names: Vec<String> = (missing.iter())
        .map(|(channel, subdir, _)| format!("{channel} for {subdir}"))
        .collect();
    names.join(", ")
}

fn missing_downloads(missing: &[(String, String, PathBuf)]) -> String {
    let lines: Vec<String> = (missing.iter())
        .map(|(channel, subdir, path)| {
            format!(
                "{}/{subdir}/repodata.json to {}",
                base(channel),
                path.display()
            )
        })
        .collect();
    lines.join(", ")
}

/// The platform of the containers of this machine as conda names it: Linux on this machine's
/// processor, like `linux-64` or `linux-aarch64`.
pub fn platform() -> String {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "64",
        "aarch64" => "aarch64",
        "powerpc64" => "ppc64le",
        other => other,
    };
    format!("linux-{arch}")
}

/// Locks an environment for a platform with the repodata in `folder`.
/// This bounded legacy search assumes common Linux virtual package names are present,
/// without checking their versions or builds. Target compatibility remains unknown.
/// Use [`lock_with_virtual`] to check authoritative target capabilities instead.
pub fn lock(environment: &Environment, folder: &Path, platform: &str) -> Result<Lock, LockError> {
    lock_inner(environment, folder, platform, None)
}

/// Locks with authoritative capabilities of the target container, including version and
/// build requirements. Omitted virtual packages are absent. No host capabilities are inferred.
pub fn lock_with_virtual(
    environment: &Environment,
    folder: &Path,
    platform: &str,
    capabilities: &[VirtualPackage],
) -> Result<Lock, LockError> {
    lock_inner(environment, folder, platform, Some(capabilities))
}

fn lock_inner(
    environment: &Environment,
    folder: &Path,
    platform: &str,
    capabilities: Option<&[VirtualPackage]>,
) -> Result<Lock, LockError> {
    let mut virtual_packages = capabilities.map(<[VirtualPackage]>::to_vec);
    if let Some(packages) = &mut virtual_packages {
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        for (at, package) in packages.iter().enumerate() {
            let valid_name = package.name.starts_with("__")
                && MatchSpec::parse(&package.name).is_ok_and(|spec| spec.name == package.name);
            if !valid_name || Version::parse(&package.version).is_none() {
                return Err(LockError::Spec(SpecError {
                    text: format!("{} {} {}", package.name, package.version, package.build),
                    reason: "invalid virtual package name or version",
                }));
            }
            if at > 0 && packages[at - 1].name == package.name {
                return Err(LockError::Spec(SpecError {
                    text: package.name.clone(),
                    reason: "duplicate virtual package name",
                }));
            }
        }
    }
    let virtual_records: Option<Vec<Record>> = virtual_packages.as_ref().map(|packages| {
        packages
            .iter()
            .map(|package| Record {
                name: package.name.clone(),
                version: package.version.clone(),
                build: package.build.clone(),
                ..Record::default()
            })
            .collect()
    });
    let channels = channels(&environment.channels);
    let index = Index::load(folder, &channels, platform)?;
    let records = solve::solve(
        &index,
        &environment.dependencies,
        virtual_records.as_deref(),
    )?;
    Ok(Lock {
        format: FORMAT,
        platform: platform.to_owned(),
        channels,
        specs: environment
            .dependencies
            .iter()
            .map(ToString::to_string)
            .collect(),
        pip: environment.pip.clone(),
        packages: records.into_iter().map(Package::of).collect(),
        virtual_packages,
    })
}
