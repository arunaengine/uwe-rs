//! Conda environments, as far as they work without network (decision 12): environment files
//! and their match specs, the order of conda versions, and the lock of an environment, which a
//! small resolver makes from the repodata of its channels in a local folder. The lock names
//! the packages by address and digest, so that an image built from it is the same anywhere.
//! Building the image, and downloading repodata and packages, need the network; see
//! docs/design/conda.md.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod lock;
mod repodata;
mod spec;
mod version;

use std::path::PathBuf;

pub use lock::{FORMAT, Lock, Package, base};
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
