//! `iunctus lock-environment <file>`: the lock of a conda environment file, made without
//! network from the repodata in `<cache>/conda/`. See docs/design/conda.md.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use conda_environment::{Environment, Lock, LockError};

use crate::arguments::Arguments;
use crate::failure::{Failure, failure};

/// The channel of an environment file that names none, as Miniforge has it.
const DEFAULT_CHANNEL: &str = "conda-forge";

/// Writes the lock of an environment file: as JSON to standard output, or to `-output`, where
/// a name that ends in `.txt` gets the explicit list that conda and micromamba install from.
pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    let Some(file) = &arguments.target else {
        return Err(Failure::usage(
            "iunctus: name the environment file, like `iunctus lock-environment envs/qc.yaml`",
        ));
    };
    let text = std::fs::read_to_string(file)
        .map_err(|error| failure(format!("iunctus: cannot read {}: {error}", file.display())))?;
    let mut environment = Environment::parse(&text)
        .map_err(|error| failure(format!("iunctus: {}: {error}", file.display())))?;
    if environment.channels.is_empty() {
        environment.channels.push(DEFAULT_CHANNEL.into());
    }
    let platform = (arguments.platform.clone()).unwrap_or_else(conda_environment::platform);
    let folder = repodata_folder();
    let lock = conda_environment::lock(&environment, &folder, &platform)
        .map_err(|error| locked(file, error))?;
    if !lock.pip.is_empty() {
        eprintln!(
            "iunctus: {}: the lock does not pin the {} packages of pip; pip installs them when \
             the image is built",
            file.display(),
            lock.pip.len()
        );
    }
    let Some(output) = &arguments.output else {
        println!("{}", json(&lock));
        return Ok(());
    };
    let explicit = output
        .extension()
        .is_some_and(|extension| extension == "txt");
    let written = match explicit {
        true => lock.explicit(),
        false => format!("{}\n", json(&lock)),
    };
    std::fs::write(output, written).map_err(|error| {
        failure(format!(
            "iunctus: cannot write {}: {error}",
            output.display()
        ))
    })?;
    println!(
        "iunctus: locked {} packages for {platform} in {}; the environment is {}",
        lock.packages.len(),
        output.display(),
        lock.digest()
    );
    Ok(())
}

/// `<cache>/conda`, where the repodata of each channel and platform is kept.
pub fn repodata_folder() -> PathBuf {
    crate::cache_directory().join("conda")
}

fn json(lock: &Lock) -> String {
    // A lock is texts and lists, which always serialize.
    serde_json::to_string_pretty(lock).unwrap_or_default()
}

/// Missing repodata is for the machine to fix; the rest is the environment's.
fn locked(file: &Path, error: LockError) -> Failure {
    let message = format!("iunctus: {}: {error}", file.display());
    match error {
        LockError::MissingRepodata { .. } | LockError::Io { .. } | LockError::Repodata { .. } => {
            Failure::setup(message)
        }
        _ => failure(message),
    }
}
