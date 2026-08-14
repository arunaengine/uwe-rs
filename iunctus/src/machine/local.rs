//! The parts of this machine that every real run needs: the container engine, the store and
//! the cache folder with their free space, and the run database.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use executor::ContainerEngine;

use super::{Report, State};
use crate::arguments::Arguments;
use crate::setup::Setup;
use crate::{cache_directory, runs};

/// Less free space than this in the store or the cache folder is too little for real runs.
const TOO_LITTLE_SPACE: u64 = 1 << 30;

/// The container engine, which the local executor and the Python container of decision 19
/// need. With another executor, one that does not answer is not set up rather than missing,
/// as only Snakefiles that import Python packages without a stand-in need it then.
pub(super) fn engine(
    runtime: &tokio::runtime::Runtime,
    setup: &Setup,
    arguments: &Arguments,
    report: &mut Report,
) {
    let part = "container engine";
    let name = setup.engine();
    let named = Some(name.as_str()).filter(|name| !name.is_empty());
    match runtime.block_on(ContainerEngine::named(named)) {
        Ok(engine) => {
            let version = runtime.block_on(engine.client().version()).ok();
            let version = version.and_then(|version| version.version);
            let (kind, place) =
                (engine.name.split_once(" at ")).unwrap_or((engine.name.as_str(), ""));
            let without_root = if engine.rootless {
                ", without root"
            } else {
                ""
            };
            let text = match version {
                Some(version) => format!("{kind} {version} at {place}{without_root}"),
                None => format!("{kind} at {place}{without_root}"),
            };
            report.add(State::Ready, part, text);
        }
        Err(error) => match arguments.executor.as_deref() {
            Some(executor @ ("tes" | "slurm" | "kubernetes")) => report.add(
                State::NotSetUp,
                part,
                format!(
                    "none answers on this machine; -executor {executor} does not need one, only \
                     Snakefiles that import Python packages without a stand-in do (decision 19)"
                ),
            ),
            _ => report.add(State::Missing, part, error.to_string()),
        },
    }
}

/// The store: a folder that iunctus can write, with its free space, or an address of a storage
/// provider, asked once whether it answers.
pub(super) fn store(
    runtime: &tokio::runtime::Runtime,
    setup: &Setup,
    arguments: &Arguments,
    report: &mut Report,
) {
    let part = "store";
    let location = setup.store_location(arguments);
    if storage::address::scheme(&location).is_some() {
        match runtime.block_on(setup.storage.stat(&location)) {
            Ok(_) => report.add(State::Ready, part, format!("{location} answers")),
            Err(error) => report.add(
                State::Missing,
                part,
                format!("{location}: {error}; check the storage provider of the config file"),
            ),
        }
        let Some(local) = setup.storage.local_store() else {
            cache_folder(None, report);
            return;
        };
        // The folder in front of the store, from `local` of `[store]`.
        let folder = PathBuf::from(local);
        match writable(&folder) {
            Ok(()) => report.add_space("store folder", &folder, "the folder of the store"),
            Err(problem) => report.add(
                State::Missing,
                "store folder",
                format!("{local}: {problem}; name another with `local` in [store]"),
            ),
        }
        cache_folder(Some(&folder), report);
        return;
    }
    let folder = PathBuf::from(&location);
    if let Err(problem) = writable(&folder) {
        report.add(
            State::Missing,
            part,
            format!(
                "{location}: {problem}; name another folder with -store or `location` in [store]"
            ),
        );
        return;
    }
    report.add_space(part, &folder, "the store");
    cache_folder(Some(&folder), report);
}

impl Report {
    /// A folder with its free space, which is missing when too little is left.
    fn add_space(&mut self, part: &str, folder: &Path, what: &str) {
        match free_space(folder) {
            Some(free) if free < TOO_LITTLE_SPACE => self.add(
                State::Missing,
                part,
                format!(
                    "{}: only {} free; free some with `iunctus clean`, or give {what} another \
                     place",
                    folder.display(),
                    bytes(free)
                ),
            ),
            Some(free) => self.add(
                State::Ready,
                part,
                format!("{}, {} free", folder.display(), bytes(free)),
            ),
            None => self.add(State::Ready, part, folder.display().to_string()),
        }
    }
}

/// The cache folder, where tasks run and remote files are downloaded, when it is not on the
/// file system of the store.
fn cache_folder(store: Option<&Path>, report: &mut Report) {
    let cache = cache_directory();
    if let Err(problem) = writable(&cache) {
        report.add(
            State::Missing,
            "cache folder",
            format!(
                "{}: {problem}; set XDG_CACHE_HOME to a folder that you can write",
                cache.display()
            ),
        );
        return;
    }
    let device = |path: &Path| std::fs::metadata(path).ok().map(|metadata| metadata.dev());
    if store.is_some_and(|store| device(store) == device(&cache)) {
        return;
    }
    report.add_space("cache folder", &cache, "the cache with XDG_CACHE_HOME");
}

/// Whether iunctus can make the folder and write into it.
fn writable(folder: &Path) -> Result<(), String> {
    std::fs::create_dir_all(folder).map_err(|error| error.to_string())?;
    let probe = folder.join(format!(".iunctus-check-{}", std::process::id()));
    std::fs::write(&probe, b"").map_err(|error| error.to_string())?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// The bytes that the user may still write on the file system of a folder.
fn free_space(folder: &Path) -> Option<u64> {
    let stat = rustix::fs::statvfs(folder).ok()?;
    Some(stat.f_bavail.saturating_mul(stat.f_frsize))
}

/// A size like `12.3 GB`.
fn bytes(size: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "kB", "MB", "GB", "TB"];
    let mut value = size as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    match unit {
        0 => format!("{size} bytes"),
        _ => format!("{value:.1} {}", UNITS[unit]),
    }
}

/// The run database, which every run fills.
pub(super) fn database(arguments: &Arguments, report: &mut Report) {
    let part = "run database";
    let path = runs::database_path(arguments);
    match runs::open(arguments) {
        Ok(database) => {
            let count = database.runs(u32::MAX).map_or(0, |runs| runs.len());
            let runs = match count {
                1 => "1 run".to_owned(),
                count => format!("{count} runs"),
            };
            report.add(State::Ready, part, format!("{}, {runs}", path.display()));
        }
        Err(failure) => report.add(
            State::Missing,
            part,
            failure.message.trim_start_matches("iunctus: "),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_in_plain_units() {
        assert_eq!(bytes(512), "512 bytes");
        assert_eq!(bytes(1_500), "1.5 kB");
        assert_eq!(bytes(123_400_000_000), "123.4 GB");
    }
}
