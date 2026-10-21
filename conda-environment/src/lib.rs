//! Parses and locks Conda environments for verified container image preparation.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod acquisition;
mod cache;
mod distribution;
mod environment;
mod fetch;
mod image;
mod lock;
mod pip;
mod pypi;
mod repodata;
mod solve;
mod spec;
mod version;

use std::path::{Path, PathBuf};

pub use acquisition::Acquisition;
pub use cache::{CachedImage, IMAGE_CACHE_NAMESPACE, ImageBuild, ImageBuildClaim, ImageCache};
pub use environment::{Environment, EnvironmentError};
pub use fetch::{fetch_packages, fetch_repodata};
pub use image::{BuiltImage, CompletedImage, IMAGE_RECEIPT_FORMAT, ImagePreparation};
pub use lock::{FORMAT, Lock, Package, PipLock, PipPackage, VirtualPackage, base};
pub use pip::{PipError, lock_pip, validate_pip};
pub use repodata::{Index, Record, channels, repodata_path, valid_channel};
pub use spec::{MatchSpec, SpecError};
pub use version::{Version, VersionSpec};

/// Why an environment cannot be locked.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error(transparent)]
    Pip(#[from] PipError),
    #[error("Conda acquisition: {0}")]
    Acquisition(String),
    #[error("Conda solver: {0}")]
    Solver(String),
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

/// Locks an environment from local repodata without inferring virtual packages.
/// Target compatibility stays unknown and capability dependencies require explicit packages.
/// Use [`lock_with_virtual`] to check authoritative target capabilities.
pub fn lock(environment: &Environment, folder: &Path, platform: &str) -> Result<Lock, LockError> {
    lock_inner(environment, folder, platform, None, folder)
}

/// Locks with authoritative capabilities of the target container, including version and
/// build requirements. Omitted virtual packages are absent. No host capabilities are inferred.
pub fn lock_with_virtual(
    environment: &Environment,
    folder: &Path,
    platform: &str,
    capabilities: &[VirtualPackage],
) -> Result<Lock, LockError> {
    lock_inner(environment, folder, platform, Some(capabilities), folder)
}

/// Resolves a local repodata snapshot while retaining the persistent offline PyPI cache.
pub fn lock_with_cache(
    environment: &Environment,
    folder: &Path,
    platform: &str,
    capabilities: &[VirtualPackage],
    cache: &Path,
) -> Result<Lock, LockError> {
    lock_inner(environment, folder, platform, Some(capabilities), cache)
}

fn lock_inner(
    environment: &Environment,
    folder: &Path,
    platform: &str,
    capabilities: Option<&[VirtualPackage]>,
    cache: &Path,
) -> Result<Lock, LockError> {
    validate_pip(&environment.pip)?;
    let specs = conda_specs(environment);
    let virtual_packages = validate_capabilities(capabilities)?;
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
    let records = solve::solve(&index, &specs, virtual_records.as_deref())?;
    let mut lock = Lock {
        format: FORMAT,
        platform: platform.to_owned(),
        channels,
        specs: environment
            .dependencies
            .iter()
            .map(ToString::to_string)
            .collect(),
        pip: environment.pip.clone(),
        pypi: None,
        packages: records.iter().map(Package::of).collect(),
        virtual_packages,
    };
    acquisition::lock_pip_cached(&mut lock, cache, true)?;
    Ok(lock)
}

pub(crate) fn conda_specs(environment: &Environment) -> Vec<MatchSpec> {
    let mut specs = environment.dependencies.clone();
    if !environment.pip.is_empty() && !specs.iter().any(|spec| spec.name == "pip") {
        specs.push(MatchSpec::parse("pip").expect("fixed pip spec"));
    }
    specs
}

pub(crate) fn validate_capabilities(
    capabilities: Option<&[VirtualPackage]>,
) -> Result<Option<Vec<VirtualPackage>>, LockError> {
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
    Ok(virtual_packages)
}
