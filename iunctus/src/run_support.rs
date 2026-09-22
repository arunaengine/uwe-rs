//! Shared cache and task-runner setup.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;

/// How the task runner treats the inputs of tasks: remote files are downloaded once into the
/// cache folder, and the digests of local files are kept there, so that a file that did not
/// change is not read again. `import_inputs` copies local inputs into the store too, for
/// executors whose tasks run on other machines.
pub(crate) fn runner_settings(
    storage: storage::Storage,
    import_inputs: bool,
) -> task_runner::Settings {
    let cache = cache_directory();
    let digests = cache.join(task_runner::DigestCache::FILE_NAME);
    task_runner::Settings {
        downloads: cache.join("downloads"),
        import_inputs,
        storage,
        // Without the cache of digests, inputs are read as before; it only saves time.
        digests: task_runner::DigestCache::open(&digests).ok(),
    }
}

/// The cache folder that `[cache] folder` of the config file names, set once at the start.
static CACHE_FOLDER: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Uses this cache folder for the whole command, in place of the one of `XDG_CACHE_HOME`.
pub(crate) fn use_cache_folder(folder: PathBuf) {
    let _ = CACHE_FOLDER.set(folder);
}

/// The folder of `[cache] folder` of the config file, else `$XDG_CACHE_HOME/iunctus`, else
/// `~/.cache/iunctus`.
pub(crate) fn cache_directory() -> PathBuf {
    if let Some(folder) = CACHE_FOLDER.get() {
        return folder.clone();
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("iunctus")
}

/// A run id from the clock and the process id.
pub(crate) fn run_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    format!("{now:x}-{:x}", std::process::id())
}
